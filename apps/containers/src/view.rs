//! The window's content, built from the toolkit's widgets.
//!
//! The sections are a [`NavList`] in the shell's sidebar; each section's page
//! is composed from tables, property lists, tabs and buttons. Nothing here
//! paints, sizes or colours anything: every control says what it is and the
//! theme decides how it looks. Interaction leaves as an [`Intent`] that the
//! app applies to the window, the draft and the engine.

use std::cell::Cell;
use std::rc::Rc;
use ui::num::u32_of_usize;

use ui::icons::Icon;
use ui::text_render::TextKind;
use ui::tokens::{SpacingStep, TextRole};
use ui::widget::data_table::{CellAlign, ColumnDef, ColumnWidth, DataTable, TableSort};
use ui::widget::toggle_group::Segment;
use ui::widget::{
    Actions, Badge, Button, ButtonRole, ChipStyle, Column, Container as Well, Element, EmptyState,
    ListRow, ModelView, NavItem, NavList, NavSection, ProgressBar, Property, PropertyColumn,
    PropertyList, Row, Scrollable, Section, SegmentedStyle, Spacer, Spinner, StatGroup, StatTile,
    StatusLine, Text, ToggleGroup, Tone, Widget,
};

use crate::model::{
    Container, ContainerStatus, DetailTab, DistroBox, Engine, Image, Section as Place, Volume,
};
use crate::podman::Action;

/// What a control in the window asks for. The app applies each one to the
/// window, the draft and the engine, the same way for a click, a key or an
/// accessibility action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Intent {
    ShowSection(Place),
    Select(Option<String>),
    ShowTab(DetailTab),
    Lifecycle {
        id: String,
        action: Action,
    },
    OpenShell(String),
    NewContainer,
    /// Pull this reference, or ask for one.
    PullImage(Option<String>),
    TagImage(String),
    RemoveImage(String),
    NewVolume,
    RemoveVolume(String),
    EnterDistrobox(String),
    SubmitCreate,
    CloseCreate,
    ConfirmAccept,
    ConfirmCancel,
    PromptSubmit,
    PromptCancel,
    ClosePull,
    /// Ask the engine again whether it is there.
    Reconnect,
}

/// The queue controls write to and the app drains.
pub(crate) type Intents = ui::Intents<Intent>;

/// The containers of `engine` in list order: running ones first, then by name.
pub(crate) fn listed_containers(engine: &Engine) -> Vec<&Container> {
    let mut list: Vec<&Container> = engine.containers.iter().collect();
    list.sort_by(|a, b| {
        b.status
            .is_running()
            .cmp(&a.status.is_running())
            .then_with(|| a.name.cmp(&b.name))
    });
    list
}

// ---- sidebar ---------------------------------------------------------------

fn place_id(place: Place) -> &'static str {
    place.as_str()
}

/// The sections of the engine, with what each holds. An engine that did
/// not answer holds nothing anyone counted, so its sections show no count.
pub(crate) fn section_list(
    engine: &Engine,
    selected: Place,
    counted: bool,
    intents: &Intents,
) -> NavList {
    let item = |place: Place, icon: Icon, n: usize| {
        let item = NavItem::new(place_id(place), place.label())
            .icon(icon)
            .without_actions();
        if counted {
            item.count(u32_of_usize(n))
        } else {
            item
        }
    };
    let items = vec![
        item(Place::Containers, Icon::Box, engine.containers.len()),
        item(Place::Images, Icon::Layers, engine.images.len()),
        item(Place::Volumes, Icon::HardDrive, engine.volumes.len()),
        item(Place::Distrobox, Icon::Terminal, engine.distroboxes.len()),
    ];
    let intents = intents.clone();
    NavList::new(vec![NavSection::untitled("sections", items)])
        .selected(place_id(selected))
        .with_id("containers-sections")
        .with_accessible_name("Sections")
        .on_select(move |id| {
            if let Some(place) = Place::parse(&id) {
                intents.push(Intent::ShowSection(place));
            }
        })
}

/// The sidebar: the sections, and at its foot whether the engine answers.
pub(crate) fn sidebar(
    engine: &Engine,
    selected: Place,
    connected: bool,
    intents: &Intents,
) -> Element {
    // Only an engine that answered is connected; the bundled sample says
    // that it is one.
    let status = if connected && engine.live {
        StatusLine::active("Connected")
    } else if connected {
        StatusLine::idle("Sample data")
    } else {
        StatusLine::warning("Not connected")
    };
    let footer = Column::new()
        .with_intrinsic_height()
        .push(Box::new(status))
        .push(Box::new(
            Text::new(&engine.connection)
                .with_role(TextRole::Caption)
                .dimmed(),
        ));
    Box::new(
        Column::new()
            .with_no_padding()
            .push(Box::new(section_list(engine, selected, connected, intents)))
            .push(Box::new(Spacer::new()))
            .push(Box::new(footer)),
    )
}

// ---- header actions ------------------------------------------------------

pub(crate) fn search_button() -> Element {
    Box::new(ui::search::search_button(
        ui::search::SearchRequest::this_app(),
    ))
}

/// The one main action of a page: an inverted button with its icon.
pub(crate) fn primary(
    id: &str,
    label: &str,
    icon: Icon,
    intent: Intent,
    intents: &Intents,
) -> Element {
    Box::new(
        Button::new(id, label)
            .with_icon(icon)
            .role(ButtonRole::Primary)
            .on_view(intents.sender(intent)),
    )
}

// ---- status ----------------------------------------------------------------

/// How a container's state reads: running is good news, a dead container
/// needs attention, a paused or restarting one is in between.
fn status_tone(status: ContainerStatus) -> Tone {
    match status {
        ContainerStatus::Running => Tone::Success,
        ContainerStatus::Paused => Tone::Warning,
        ContainerStatus::Restarting | ContainerStatus::Created => Tone::Info,
        ContainerStatus::Dead => Tone::Danger,
        ContainerStatus::Exited => Tone::Neutral,
    }
}

fn status_badge(status: ContainerStatus) -> Badge {
    let badge = Badge::text(status.label());
    match status_tone(status) {
        Tone::Neutral => badge,
        tone => badge.style(ChipStyle::Tinted(tone)),
    }
}

/// The lifecycle verbs a container offers in its current state.
pub(crate) fn lifecycle(status: ContainerStatus) -> Vec<Action> {
    if status.is_running() {
        vec![Action::Stop, Action::Restart]
    } else if status == ContainerStatus::Paused {
        vec![Action::Unpause, Action::Stop]
    } else {
        vec![Action::Start, Action::Remove]
    }
}

fn action_icon(action: Action) -> Icon {
    match action {
        Action::Start | Action::Unpause => Icon::Play,
        Action::Stop => Icon::Square,
        Action::Restart => Icon::RotateCw,
        Action::Pause => Icon::Pause,
        Action::Remove => Icon::Trash,
    }
}

fn verb(action: Action) -> &'static str {
    match action {
        Action::Start => "start",
        Action::Stop => "stop",
        Action::Restart => "restart",
        Action::Pause => "pause",
        Action::Unpause => "unpause",
        Action::Remove => "remove",
    }
}

// ---- containers ------------------------------------------------------------

/// A row's status dot for a container state.
fn with_status(row: ListRow, status: ContainerStatus) -> ListRow {
    match status {
        ContainerStatus::Running => row.active(),
        ContainerStatus::Paused | ContainerStatus::Restarting | ContainerStatus::Created => {
            row.warning()
        }
        ContainerStatus::Dead => row.error(),
        ContainerStatus::Exited => row.idle(),
    }
}

/// A row action that becomes `intent`.
fn row_action(
    row: ListRow,
    icon: Icon,
    label: String,
    intent: Intent,
    intents: &Intents,
) -> ListRow {
    row.action(icon, label, intents.sender(intent))
}

/// One container as the list shows it.
#[derive(Clone)]
struct ContainerRow {
    id: String,
    name: String,
    image: String,
    status: ContainerStatus,
}

/// Running containers first, then the ones paused or on their way, then
/// the stopped ones.
fn status_rank(status: ContainerStatus) -> u8 {
    match status {
        ContainerStatus::Running => 0,
        ContainerStatus::Restarting | ContainerStatus::Paused | ContainerStatus::Created => 1,
        ContainerStatus::Exited => 2,
        ContainerStatus::Dead => 3,
    }
}

/// The containers in a table: name, image and state, sortable from the
/// headers in the order `sort` keeps. A row opens its container; its
/// lifecycle verbs are the detail's.
pub(crate) fn container_list(
    engine: &Engine,
    selected: Option<&str>,
    sort: &ui::LocalState<Option<TableSort>>,
    intents: &Intents,
) -> Element {
    let listed = listed_containers(engine);
    if listed.is_empty() {
        return containers_empty(engine.live, intents);
    }
    let rows: Vec<ContainerRow> = listed
        .iter()
        .map(|c| ContainerRow {
            id: c.id.clone(),
            name: c.name.clone(),
            // The image by its own name; its registry and path are in the
            // detail, and would leave the narrow column only an ellipsis.
            image: c
                .image
                .rsplit_once('/')
                .map_or(c.image.as_str(), |(_, name)| name)
                .to_owned(),
            status: c.status,
        })
        .collect();
    let ids: Vec<String> = rows.iter().map(|row| row.id.clone()).collect();
    let select = intents.clone();
    let open = intents.clone();
    let opened = ids.clone();
    let mut table = DataTable::new(vec![
        ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0))
            .sortable(true)
            .with_text(|row: &ContainerRow| row.name.clone()),
        ColumnDef::new("image", "Image", ColumnWidth::Flex(1.0))
            .sortable(true)
            .priority(1)
            .with_kind(TextKind::Identifier)
            .with_text(|row: &ContainerRow| row.image.clone()),
        // The state in words on its tone, so it never rests on colour.
        ColumnDef::new("state", "State", ColumnWidth::Auto)
            .sorted_by(|a: &ContainerRow, b: &ContainerRow| {
                status_rank(a.status).cmp(&status_rank(b.status))
            })
            .with_align(CellAlign::Center)
            .with_badge(|row: &ContainerRow| {
                (row.status.label().to_string(), status_tone(row.status))
            }),
    ])
    .with_id("containers-list")
    .with_accessible_name("Containers")
    .with_sort_state(sort)
    .selectable(true)
    .on_row_click(move |index, _, _| {
        if let Some(id) = ids.get(index) {
            select.push(Intent::Select(Some(id.clone())));
        }
    })
    .on_activate(move |index| {
        if let Some(id) = opened.get(index) {
            open.push(Intent::Select(Some(id.clone())));
        }
    });
    let chosen = selected.and_then(|id| rows.iter().position(|row| row.id == id));
    table.set_rows(rows);
    if let Some(index) = chosen {
        table.select_row(index);
    }
    list_column(Box::new(table))
}

/// A list in the list column: its rows' text on the column's content edge
/// (the column gutter, where the column's title starts), their lift
/// bleeding into the inset.
fn list_column(list: Element) -> Element {
    Box::new(Stack::new(SpacingStep::S0, vec![list]).on_column_edge())
}

fn containers_empty(live: bool, intents: &Intents) -> Element {
    if live {
        Box::new(
            EmptyState::new("No containers")
                .with_icon(Icon::Box)
                .with_message("Create a container from an image to run it here.")
                .with_action("New container")
                .on_view(intents.sender(Intent::NewContainer)),
        )
    } else {
        engine_missing(intents)
    }
}

/// Podman did not answer: say so, once, and offer to ask again.
pub(crate) fn engine_missing(intents: &Intents) -> Element {
    Box::new(
        EmptyState::new("Podman isn't available")
            .with_icon(Icon::Box)
            .with_message("Install Podman or start its user service to manage containers here.")
            .with_action("Try again")
            .on_effect(intents.sender(Intent::Reconnect)),
    )
}

/// The content beside the list while nothing is chosen.
pub(crate) fn nothing_selected() -> Element {
    Box::new(
        EmptyState::new("No container selected")
            .with_icon(Icon::Box)
            .with_message("Choose a container to see its logs, usage and settings."),
    )
}

/// A stack whose gap and insets are spacing steps, resolved from the theme
/// at layout so they follow the spacing scale.
struct Stack {
    column: Column,
    gap: SpacingStep,
    inset_x: SpacingStep,
    inset_y: SpacingStep,
    /// Inset sideways by the column gutter instead of `inset_x`.
    column_edge: bool,
}

impl Stack {
    fn new(gap: SpacingStep, children: Vec<Element>) -> Self {
        let mut column = Column::new().with_intrinsic_height().with_no_padding();
        for child in children {
            column = column.push(child);
        }
        Self {
            column,
            gap,
            inset_x: SpacingStep::S0,
            inset_y: SpacingStep::S0,
            column_edge: false,
        }
    }

    /// A title and the line that belongs to it, one `s1` step apart.
    fn tight(children: Vec<Element>) -> Self {
        Self::new(SpacingStep::S1, children)
    }

    /// Inset the stack's content from its edges.
    fn inset(mut self, x: SpacingStep, y: SpacingStep) -> Self {
        self.inset_x = x;
        self.inset_y = y;
        self
    }

    /// Stand the content on the column's content edge, where the shell
    /// starts the column's title.
    fn on_column_edge(mut self) -> Self {
        self.column_edge = true;
        self
    }
}

impl ui::widget::Decorator for Stack {
    type Inner = Column;

    fn inner(&self) -> &Column {
        &self.column
    }

    fn inner_mut(&mut self) -> &mut Column {
        &mut self.column
    }

    fn build_layout(
        &mut self,
        taffy: &mut ui::taffy::TaffyTree<()>,
        theme: &ui::theme::Theme,
        node_map: &mut std::collections::HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        let spacing = &theme.tokens.spacing;
        let x = if self.column_edge {
            ui::num::f32_of_f64(ui::chrome::column_gutter(theme))
        } else {
            spacing.step(self.inset_x)
        };
        let y = spacing.step(self.inset_y);
        self.column = std::mem::take(&mut self.column).with_horizontal_padding(x, x);
        self.column.set_vertical_padding(y);
        self.column.set_gap(spacing.step(self.gap));
        self.column.build_layout(taffy, theme, node_map)
    }
}

/// Everything the detail of one container needs.
pub(crate) struct DetailModel<'a> {
    pub container: &'a Container,
    pub tab: DetailTab,
    pub controls: bool,
    /// A live engine answers the container's published actions; the sample
    /// answers through the app.
    pub live_engine: bool,
}

/// The chosen container: what it is, what can be done to it, and one tab
/// of detail.
pub(crate) fn container_detail(model: &DetailModel<'_>, intents: &Intents) -> Element {
    let c = model.container;
    let title = Stack::tight(vec![
        Box::new(
            Text::new(&c.name)
                .with_role(TextRole::Title)
                .with_kind(TextKind::Identifier),
        ),
        Box::new(
            Text::new(&c.image)
                .with_role(TextRole::Caption)
                .with_kind(TextKind::Identifier)
                .dimmed(),
        ),
    ]);
    let mut page = Column::new()
        .with_intrinsic_height()
        .push(Box::new(title))
        .push(Box::new(detail_actions(model, intents)))
        .push(Box::new(tabs(model.tab, intents)));
    page = page.push(match model.tab {
        DetailTab::Logs => logs(c),
        DetailTab::Stats => stats(c),
        DetailTab::Inspect => inspect(c),
        DetailTab::Terminal => shell(model, intents),
    });
    Box::new(page)
}

/// A button that runs `verb` on the container. A live container's verbs
/// are its published actions, so the host checks permission and asks for
/// confirmation exactly as it does for search and agents; the sample (and
/// removing, which asks in the window first) goes through the app.
fn verb_button(
    model: &DetailModel<'_>,
    verb_id: &str,
    label: &str,
    icon: Icon,
    intent: Intent,
    intents: &Intents,
) -> Button {
    let c = model.container;
    let button = Button::new(&format!("detail-{verb_id}-{}", c.id), label)
        .with_icon(icon)
        .with_enabled(model.controls);
    let binding = (model.live_engine && verb_id != "remove")
        .then(|| crate::app::container_row(c, true).into_resource())
        .and_then(|resource| ui::tilo_actions::ActionBinding::unit_for(&resource, verb_id).ok());
    match binding {
        Some(binding) => button.on_action(binding),
        None => button.on_view(intents.sender(intent)),
    }
}

fn detail_actions(model: &DetailModel<'_>, intents: &Intents) -> Row {
    let c = model.container;
    let mut verbs = lifecycle(c.status);
    if !verbs.contains(&Action::Remove) {
        verbs.push(Action::Remove);
    }
    let button = |action: Action| {
        verb_button(
            model,
            verb(action),
            action.label(),
            action_icon(action),
            Intent::Lifecycle {
                id: c.id.clone(),
                action,
            },
            intents,
        )
        .role(if action == Action::Remove {
            ButtonRole::Destructive
        } else {
            ButtonRole::Secondary
        })
    };
    // The buttons are one group: in a narrow column they move under the
    // state together instead of breaking apart.
    let (last, rest) = verbs
        .split_last()
        .map_or((Action::Remove, &[][..]), |(l, r)| (*l, r));
    let mut buttons = Actions::new(button(last));
    for action in rest {
        buttons = buttons.secondary(button(*action));
    }
    Row::new()
        .push(Box::new(status_badge(c.status)))
        .push(Box::new(
            Text::new(&c.status_detail)
                .with_role(TextRole::Caption)
                .dimmed(),
        ))
        .push(Box::new(Spacer::new()))
        .push(Box::new(buttons))
}

fn tabs(selected: DetailTab, intents: &Intents) -> ToggleGroup {
    let intents = intents.clone();
    let index = DetailTab::ALL
        .iter()
        .position(|tab| *tab == selected)
        .unwrap_or(0);
    ToggleGroup::new(
        DetailTab::ALL
            .iter()
            .map(|tab| Segment::new(tab.label()))
            .collect(),
        index,
    )
    .with_style(SegmentedStyle::Tabs)
    .on_view("container-tabs", move |i| {
        if let Some(tab) = DetailTab::ALL.get(i) {
            intents.push(Intent::ShowTab(*tab));
        }
    })
}

/// One sentence where a tab has nothing to show yet.
fn note(text: &str) -> Element {
    Box::new(Text::new(text).with_role(TextRole::Body).dimmed())
}

/// How serious a log line is, read from the level word it carries
/// (`WARN`, `[error]`, `level=warning`, `ERROR:`). Lines without one are
/// plain output.
pub(crate) fn line_severity(line: &str) -> Tone {
    let mut tone = Tone::Neutral;
    let words = line.split(|c: char| c.is_whitespace() || matches!(c, '=' | '[' | ']' | ':'));
    for word in words {
        match word.to_ascii_lowercase().as_str() {
            "error" | "err" | "fatal" | "panic" | "crit" | "critical" | "emerg" | "alert" => {
                return Tone::Danger;
            }
            "warn" | "warning" => tone = Tone::Warning,
            _ => {}
        }
    }
    tone
}

/// One line of output, in the ink of its severity: a warning or an error
/// reads at a glance, as it does in a terminal.
fn log_line(line: &str) -> Element {
    let text = Text::new(line).with_role(TextRole::Code);
    Box::new(match line_severity(line) {
        Tone::Neutral => text,
        tone => text.tone(tone),
    })
}

fn logs(c: &Container) -> Element {
    if let Some(error) = &c.logs_error {
        return note(&format!("Log output is unavailable: {error}"));
    }
    if c.logs.is_empty() {
        return note("The container hasn't written any log output.");
    }
    output(c.logs.iter().map(String::as_str))
}

/// Output as the process wrote it, in a well: one entry per line, a line
/// wider than the column wrapping onto the next rather than scrolling
/// sideways, so every line reads whole at any width.
fn output<'a>(lines: impl Iterator<Item = &'a str>) -> Element {
    let lines: Vec<Element> = lines.map(log_line).collect();
    Box::new(
        Well::new()
            .with_background()
            .with_child(Box::new(Stack::new(SpacingStep::S0, lines))),
    )
}

/// One measured share: its name, its reading and the bar.
fn usage(label: &str, reading: &str, fraction: f32) -> Element {
    Box::new(Stack::tight(vec![
        line(label, reading),
        Box::new(ProgressBar::new(fraction.clamp(0.0, 1.0)).with_label(label)),
    ]))
}

/// A name and its reading on one line, the reading at the trailing edge.
fn line(label: &str, reading: &str) -> Element {
    Box::new(
        Row::new()
            .no_wrap()
            .push(Box::new(Text::new(label).with_role(TextRole::Label)))
            .push(Box::new(Spacer::new()))
            .push(Box::new(
                Text::new(reading)
                    .with_role(TextRole::Caption)
                    .numeric()
                    .dimmed(),
            )),
    )
}

/// A tile for an amount of data: the figure, and its unit beside it.
fn bytes_tile(label: &str, bytes: u64) -> Element {
    let formatted = ui::format::bytes(bytes);
    let tile = match formatted.rsplit_once(' ') {
        Some((value, unit)) => StatTile::new(label, value).unit(unit),
        None => StatTile::new(label, formatted.as_str()),
    };
    Box::new(tile)
}

fn stats(c: &Container) -> Element {
    if !c.status.is_running() {
        return note("Usage appears while the container runs.");
    }
    let s = &c.stats;
    if !s.available {
        return note("Usage is unavailable. Podman did not return a telemetry sample.");
    }
    let cpu = format!("{:.0}%", s.cpu_percent * 100.0);
    let memory = if s.mem_limit_bytes > 0 {
        format!(
            "{} of {}",
            ui::format::bytes(s.mem_bytes),
            ui::format::bytes(s.mem_limit_bytes)
        )
    } else {
        ui::format::bytes(s.mem_bytes)
    };
    let tiles: Vec<Element> = vec![
        bytes_tile("Received", s.net_rx_bytes),
        bytes_tile("Sent", s.net_tx_bytes),
        bytes_tile("Read from disk", s.block_read_bytes),
        bytes_tile("Written to disk", s.block_write_bytes),
    ];
    stack(vec![
        Section::titled(
            "Usage",
            vec![
                usage("CPU", &cpu, s.cpu_percent),
                usage("Memory", &memory, s.mem_fraction()),
                line("Processes", &s.pids.to_string()),
            ],
        ),
        Section::titled("Activity", vec![Box::new(StatGroup::new(tiles))]),
    ])
}

/// Sections of one tab, a group's gap apart.
fn stack(sections: Vec<Section>) -> Element {
    let mut column = Column::new().with_intrinsic_height().with_no_padding();
    for (index, section) in sections.into_iter().enumerate() {
        if index > 0 {
            // Two sibling gaps either side of nothing: the gap between groups.
            column = column.push(Box::new(Spacer::token(SpacingStep::S0)));
        }
        column = column.push(Box::new(section));
    }
    Box::new(column)
}

/// A list of facts whose values start where the other lists' do.
fn facts(column: &PropertyColumn, rows: Vec<Property>) -> Element {
    Box::new(PropertyList::new(rows).shared_column(column))
}

fn inspect(c: &Container) -> Element {
    let column = PropertyColumn::new();
    let mut page = vec![Section::titled(
        "Details",
        vec![facts(
            &column,
            vec![
                Property::new("ID", c.short_id()).with_kind(TextKind::Identifier),
                Property::new("Command", c.command.clone()).with_kind(TextKind::Code),
                Property::new("Created", c.created.clone()),
                Property::new("State", c.status_detail.clone()),
            ],
        )],
    )];
    if !c.ports.is_empty() {
        page.push(Section::titled(
            "Ports",
            vec![facts(
                &column,
                c.ports
                    .iter()
                    .map(|p| {
                        Property::new(
                            format!("{}/{}", p.container_port, p.protocol),
                            format!("Host port {}", p.host_port),
                        )
                        .with_kind(TextKind::Numeric)
                    })
                    .collect(),
            )],
        ));
    }
    if !c.mounts.is_empty() {
        page.push(Section::titled(
            "Mounts",
            vec![facts(
                &column,
                c.mounts
                    .iter()
                    .map(|m| {
                        let source = if m.read_only {
                            format!("{} (read-only)", m.source)
                        } else {
                            m.source.clone()
                        };
                        Property::new(m.destination.clone(), source).with_kind(TextKind::Identifier)
                    })
                    .collect(),
            )],
        ));
    }
    if !c.env.is_empty() {
        page.push(Section::titled(
            "Environment",
            vec![facts(
                &column,
                c.env
                    .iter()
                    .map(|(k, v)| Property::new(k.clone(), v.clone()).with_kind(TextKind::Code))
                    .collect(),
            )],
        ));
    }
    if !c.labels.is_empty() {
        page.push(Section::titled(
            "Labels",
            vec![facts(
                &column,
                c.labels
                    .iter()
                    .map(|(k, v)| {
                        Property::new(k.clone(), v.clone()).with_kind(TextKind::Identifier)
                    })
                    .collect(),
            )],
        ));
    }
    stack(page)
}

fn shell(model: &DetailModel<'_>, intents: &Intents) -> Element {
    let c = model.container;
    let (text, button) = if c.status.is_running() {
        (
            "Opens /bin/sh inside the container in a new terminal window.",
            verb_button(
                model,
                "shell",
                "Open shell",
                Icon::Terminal,
                Intent::OpenShell(c.id.clone()),
                intents,
            ),
        )
    } else {
        (
            "Start the container to open a shell in it.",
            verb_button(
                model,
                "start",
                "Start",
                Icon::Play,
                Intent::Lifecycle {
                    id: c.id.clone(),
                    action: Action::Start,
                },
                intents,
            ),
        )
    };
    Box::new(
        Column::new()
            .with_intrinsic_height()
            .with_no_padding()
            .push(note(text))
            .push(Box::new(Row::new().push(Box::new(button)))),
    )
}

// ---- images ----------------------------------------------------------------

fn used_by(n: u32) -> String {
    match n {
        0 => "Not used by any container".into(),
        1 => "Used by 1 container".into(),
        n => format!("Used by {n} containers"),
    }
}

/// One page of a section: a sentence about the whole, then its list, both
/// on the column's content edge.
fn page(summary: Option<String>, rows: Vec<Element>) -> Element {
    let mut children: Vec<Element> = Vec::new();
    if let Some(summary) = summary {
        children.push(Box::new(
            Text::new(&summary).with_role(TextRole::Body).dimmed(),
        ));
    }
    children.push(Box::new(Section::new(rows)));
    Box::new(Stack::new(SpacingStep::S3, children).inset(SpacingStep::S3, SpacingStep::S3))
}

pub(crate) fn images_page(engine: &Engine, controls: bool, intents: &Intents) -> Element {
    if engine.images.is_empty() {
        if !engine.live && !controls {
            return engine_missing(intents);
        }
        return Box::new(
            EmptyState::new("No images")
                .with_icon(Icon::Layers)
                .with_message("Pull an image to create containers from it.")
                .with_action("Pull image")
                .on_view(intents.sender(Intent::PullImage(None))),
        );
    }
    let rows: Vec<Element> = engine
        .images
        .iter()
        .map(|image: &Image| {
            let r = image.reference();
            let mut row = ListRow::new(format!("image-{r}"), r.clone())
                .icon(Icon::Layers)
                .secondary(used_by(image.in_use_by))
                .meta(ui::format::bytes(image.size_bytes));
            if controls {
                row = row_action(
                    row,
                    Icon::Download,
                    format!("Pull {r} again"),
                    Intent::PullImage(Some(r.clone())),
                    intents,
                );
                row = row_action(
                    row,
                    Icon::Copy,
                    format!("Add a tag to {r}"),
                    Intent::TagImage(r.clone()),
                    intents,
                );
                row = row_action(
                    row,
                    Icon::Trash,
                    format!("Remove {r}"),
                    Intent::RemoveImage(r),
                    intents,
                );
            }
            Box::new(row) as Element
        })
        .collect();
    let summary = format!(
        "{} {} use {} of disk space.",
        engine.images.len(),
        if engine.images.len() == 1 {
            "image"
        } else {
            "images"
        },
        ui::format::bytes(engine.image_disk_bytes())
    );
    page(Some(summary), rows)
}

// ---- volumes ---------------------------------------------------------------

pub(crate) fn volumes_page(engine: &Engine, controls: bool, intents: &Intents) -> Element {
    if engine.volumes.is_empty() {
        if !engine.live && !controls {
            return engine_missing(intents);
        }
        return Box::new(
            EmptyState::new("No volumes")
                .with_icon(Icon::HardDrive)
                .with_message("A volume keeps a container's data when the container is removed.")
                .with_action("New volume")
                .on_view(intents.sender(Intent::NewVolume)),
        );
    }
    let rows: Vec<Element> = engine
        .volumes
        .iter()
        .map(|v: &Volume| {
            let mut row = ListRow::new(format!("volume-{}", v.name), v.name.clone())
                .icon(Icon::HardDrive)
                .secondary(used_by(v.in_use_by));
            if controls {
                row = row_action(
                    row,
                    Icon::Trash,
                    format!("Remove {}", v.name),
                    Intent::RemoveVolume(v.name.clone()),
                    intents,
                );
            }
            Box::new(row) as Element
        })
        .collect();
    let in_use = engine.volumes.iter().filter(|v| v.in_use_by > 0).count();
    let summary = format!("{in_use} of {} volumes are in use.", engine.volumes.len());
    page(Some(summary), rows)
}

// ---- distrobox -------------------------------------------------------------

pub(crate) fn distrobox_page(engine: &Engine, controls: bool, intents: &Intents) -> Element {
    if engine.distroboxes.is_empty() {
        if !engine.live && !controls {
            return engine_missing(intents);
        }
        return Box::new(
            EmptyState::new("No distroboxes")
                .with_icon(Icon::Terminal)
                .with_message("Boxes you create with distrobox appear here."),
        );
    }
    let rows: Vec<Element> = engine
        .distroboxes
        .iter()
        .map(|b: &DistroBox| {
            let mut row = ListRow::new(format!("distrobox-{}", b.name), b.name.clone())
                .icon(Icon::Terminal)
                .secondary(b.image.clone())
                .meta(b.status.label());
            row = with_status(row, b.status);
            if controls {
                row = row_action(
                    row,
                    Icon::Terminal,
                    format!("Enter {}", b.name),
                    Intent::EnterDistrobox(b.name.clone()),
                    intents,
                )
                .on_activate(intents.sender(Intent::EnterDistrobox(b.name.clone())));
            }
            Box::new(row) as Element
        })
        .collect();
    page(None, rows)
}

// ---- pull progress ---------------------------------------------------------

/// The body of the pull sheet: the state of the pull and its latest output.
pub(crate) fn pull_body(pull: &crate::app::PullProgress) -> Column {
    let state: Element = match &pull.failed {
        Some(error) => Box::new(Text::new(error).with_role(TextRole::Body)),
        None if pull.done => Box::new(
            Text::new("The image is ready to create containers from.")
                .with_role(TextRole::Body)
                .dimmed(),
        ),
        // A pull reports layers, not a share of the whole: it spins
        // instead of pretending to a fraction.
        None => Box::new(
            Row::new()
                .no_wrap()
                .push(Box::new(Spinner::new()))
                .push(Box::new(
                    Text::new("Downloading layers")
                        .with_role(TextRole::Body)
                        .dimmed(),
                )),
        ),
    };
    let skip = pull.lines.len().saturating_sub(8);
    Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .push(state)
        .push(output(pull.lines.iter().skip(skip).map(String::as_str)))
}

// ---- the window ------------------------------------------------------------

/// What the window shows, published by the app whenever the engine or the
/// view changes. Each part of the window rebuilds only when the slice of
/// this it shows changed.
#[derive(Clone, Default)]
pub(crate) struct Model {
    pub engine: Rc<Engine>,
    pub view: crate::app::ViewState,
    /// Lifecycle controls are live (a connected engine, or the sample).
    pub controls: bool,
    /// The sidebar reports the engine as reachable.
    pub connected: bool,
    /// Podman did not answer a live app: the pages say so instead of
    /// showing empty lists.
    pub missing: bool,
    /// Digests of what each column shows, taken once per publish so the
    /// columns compare a hash instead of the engine on every frame.
    pub list_digest: Digest,
    pub detail_digest: Digest,
    pub page_digest: Digest,
}

/// A digest of what one part of the window shows.
pub(crate) type Digest = [u8; 32];

fn digest(value: &impl serde::Serialize) -> Digest {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    *blake3::hash(&bytes).as_bytes()
}

impl Model {
    /// Take the digests of what each column shows.
    pub(crate) fn digested(mut self) -> Self {
        let e = &self.engine;
        let rows: Vec<(&str, &str, ContainerStatus)> = listed_containers(e)
            .into_iter()
            .map(|c| (c.id.as_str(), c.name.as_str(), c.status))
            .collect();
        self.list_digest = digest(&rows);
        let chosen = self.view.selected.as_deref().and_then(|id| e.find(id));
        self.detail_digest = digest(&chosen);
        self.page_digest = match self.view.section {
            Place::Images => digest(&e.images),
            Place::Volumes => digest(&e.volumes),
            Place::Distrobox => digest(&e.distroboxes),
            Place::Containers => Digest::default(),
        };
        self
    }
}

/// The scroll positions the pages keep across rebuilds.
#[derive(Clone, Default)]
pub(crate) struct Offsets {
    pub list: Rc<Cell<(f32, f32)>>,
    pub detail: Rc<Cell<(f32, f32)>>,
}

/// Everything a part of the window is built from.
#[derive(Clone)]
pub(crate) struct Parts {
    pub model: ui::LocalState<Model>,
    pub pane: ui::LocalState<ui::widget::NavPane>,
    pub intents: Intents,
    pub offsets: Offsets,
    /// The container table's order, which the table keeps here.
    pub sort: ui::LocalState<Option<TableSort>>,
}

/// The window: a shell per section, its columns rebuilt from the model.
pub(crate) fn window(parts: &Parts) -> ModelView<(Place, bool, bool)> {
    let p = parts.clone();
    ModelView::from_state_by(
        &parts.model,
        |m: &Model| (m.view.section, m.controls, m.missing),
        move |m: &Model| -> Element { Box::new(window_shell(m, &p)) },
    )
}

fn window_shell(m: &Model, parts: &Parts) -> ui::widget::NavShell {
    let section = m.view.section;
    let intents = &parts.intents;
    let shell = ui::widget::NavShell::new()
        .with_id("containers-shell")
        .with_pane_state(&parts.pane)
        .sidebar(sidebar_view(parts));
    if m.missing {
        // Nothing to list or search: the page says why, in one column.
        return shell
            .content(page_view(parts))
            .content_header(section.label(), Vec::new());
    }
    let mut actions = vec![search_button()];
    if section == Place::Containers {
        if m.controls {
            // The list column is narrow; its one action is a glyph so the
            // title keeps its room beside the sidebar toggle.
            actions.push(Box::new(
                Button::icon_only("containers-new", Icon::Plus, "New container")
                    .role(ButtonRole::Primary)
                    .on_view(intents.sender(Intent::NewContainer)),
            ));
        }
        return shell
            .overview(list_view(parts))
            .content(detail_view(parts))
            .overview_header(section.label(), actions)
            .content_header("", vec![close_detail(parts)]);
    }
    let main = match section {
        Place::Images => Some((
            "images-pull",
            "Pull image",
            Icon::Download,
            Intent::PullImage(None),
        )),
        Place::Volumes => Some(("volumes-new", "New volume", Icon::Plus, Intent::NewVolume)),
        Place::Containers | Place::Distrobox => None,
    };
    if let Some((id, label, icon, intent)) = main
        && m.controls
    {
        actions.push(primary(id, label, icon, intent, intents));
    }
    shell
        .content(page_view(parts))
        .content_header(section.label(), actions)
}

/// The detail's close control, while a container is chosen.
fn close_detail(parts: &Parts) -> Element {
    let intents = parts.intents.clone();
    Box::new(ModelView::from_state_by(
        &parts.model,
        |m: &Model| m.view.selected.is_some(),
        move |m: &Model| -> Element {
            if m.view.selected.is_some() {
                Box::new(
                    Button::icon_only("detail-close", Icon::X, "Close details")
                        .on_view(intents.sender(Intent::Select(None))),
                )
            } else {
                Box::new(Spacer::token(SpacingStep::S0))
            }
        },
    ))
}

fn sidebar_view(parts: &Parts) -> Element {
    let intents = parts.intents.clone();
    Box::new(ModelView::from_state_by(
        &parts.model,
        |m: &Model| {
            let e = &m.engine;
            (
                m.view.section,
                [
                    e.containers.len(),
                    e.images.len(),
                    e.volumes.len(),
                    e.distroboxes.len(),
                ],
                e.connection.clone(),
                e.live,
                m.connected,
            )
        },
        move |m: &Model| sidebar(&m.engine, m.view.section, m.connected, &intents),
    ))
}

/// A column that scrolls what `view` rebuilds. The viewport is the
/// column's own, so it stays outside the rebuilt part.
fn scrolling(view: Element, offset: &Rc<Cell<(f32, f32)>>) -> Element {
    Box::new(
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_content_min_viewport_height()
            .with_persisted_offset(Rc::clone(offset))
            .with_child(view),
    )
}

fn list_view(parts: &Parts) -> Element {
    let intents = parts.intents.clone();
    let sort = parts.sort.clone();
    let view = Box::new(ModelView::from_state_by(
        &parts.model,
        |m: &Model| {
            (
                m.list_digest,
                m.view.selected.clone(),
                m.controls,
                m.missing,
            )
        },
        move |m: &Model| {
            if m.missing {
                return engine_missing(&intents);
            }
            container_list(&m.engine, m.view.selected.as_deref(), &sort, &intents)
        },
    ));
    scrolling(view, &parts.offsets.list)
}

fn detail_view(parts: &Parts) -> Element {
    let intents = parts.intents.clone();
    let view = Box::new(ModelView::from_state_by(
        &parts.model,
        |m: &Model| (m.detail_digest, m.view.tab, m.controls, m.missing),
        move |m: &Model| {
            let chosen = m.view.selected.as_deref().and_then(|id| m.engine.find(id));
            match chosen {
                Some(container) => container_detail(
                    &DetailModel {
                        container,
                        tab: m.view.tab,
                        controls: m.controls,
                        live_engine: m.engine.live,
                    },
                    &intents,
                ),
                None if m.missing => Box::new(Spacer::new()),
                None => nothing_selected(),
            }
        },
    ));
    scrolling(view, &parts.offsets.detail)
}

fn page_view(parts: &Parts) -> Element {
    let intents = parts.intents.clone();
    let view = Box::new(ModelView::from_state_by(
        &parts.model,
        |m: &Model| (m.view.section, m.page_digest, m.controls, m.missing),
        move |m: &Model| {
            if m.missing {
                return engine_missing(&intents);
            }
            match m.view.section {
                Place::Images => images_page(&m.engine, m.controls, &intents),
                Place::Volumes => volumes_page(&m.engine, m.controls, &intents),
                _ => distrobox_page(&m.engine, m.controls, &intents),
            }
        },
    ));
    scrolling(view, &parts.offsets.list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_telemetry_and_logs_are_not_presented_as_measured_zero_or_empty() {
        let engine = crate::model::sample_engine();
        let mut container = engine
            .containers
            .into_iter()
            .find(|item| item.status.is_running())
            .unwrap();
        container.stats = crate::model::Stats::default();
        let usage = stats(&container).accessibility().unwrap().name.unwrap();
        assert!(usage.contains("unavailable"), "{usage}");
        assert!(!usage.contains("0%"), "{usage}");
        container.logs.clear();
        container.logs_error = Some("engine connection closed".into());
        let failed = logs(&container).accessibility().unwrap().name.unwrap();
        assert!(failed.contains("engine connection closed"), "{failed}");
        assert!(!failed.contains("hasn't written"), "{failed}");
        container.logs_error = None;
        let empty = logs(&container).accessibility().unwrap().name.unwrap();
        assert!(empty.contains("hasn't written"), "{empty}");
    }
}
