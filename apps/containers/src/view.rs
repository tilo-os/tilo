//! The window's content, built from the toolkit's widgets.
//!
//! The sections are a [`NavList`] in the shell's sidebar; each section's page
//! is composed from tables, property lists, tabs and buttons. Nothing here
//! paints, sizes or colours anything: every control says what it is and the
//! theme decides how it looks. Interaction leaves as an [`Intent`] that the
//! app applies to the shared document and to the engine.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ui::icons::Icon;
use ui::text_render::TextKind;
use ui::tokens::TextRole;
use ui::widget::toggle_group::Segment;
use ui::widget::{
    Actions, Badge, Button, ButtonRole, ChipStyle, Column, Container as Well, Element, EmptyState,
    ListRow, ModelView, NavItem, NavList, NavSection, ProgressBar, Property, PropertyList, Row,
    Scrollable, Section, SegmentedStyle, Spacer, StatGroup, StatTile, Text, ToggleGroup, Tone,
    Widget,
};

use crate::doc::{DetailTab, Section as Place};
use crate::model::{Container, ContainerStatus, DistroBox, Engine, Image, Volume};
use crate::podman::Action;

/// What a control in the window asks for. The app applies each one to the
/// shared document and the engine, the same way for a click, a key or an
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
    /// A create-form field changed its text.
    EditDraft(crate::doc::CreateField, String),
    SubmitCreate,
    CloseCreate,
    ConfirmAccept,
    ConfirmCancel,
    PromptSubmit,
    PromptCancel,
    ClosePull,
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

    fn on(&self, intent: Intent) -> impl FnMut() + 'static {
        let intents = self.clone();
        move || intents.push(intent.clone())
    }
}

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

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The sections of the engine, with what each holds.
pub(crate) fn section_list(engine: &Engine, selected: Place, intents: &Intents) -> NavList {
    let items = vec![
        NavItem::new(place_id(Place::Containers), Place::Containers.label())
            .icon(Icon::Archive)
            .count(count(engine.containers.len()))
            .without_actions(),
        NavItem::new(place_id(Place::Images), Place::Images.label())
            .icon(Icon::Layers)
            .count(count(engine.images.len()))
            .without_actions(),
        NavItem::new(place_id(Place::Volumes), Place::Volumes.label())
            .icon(Icon::HardDrive)
            .count(count(engine.volumes.len()))
            .without_actions(),
        NavItem::new(place_id(Place::Distrobox), Place::Distrobox.label())
            .icon(Icon::Terminal)
            .count(count(engine.distroboxes.len()))
            .without_actions(),
    ];
    let intents = intents.clone();
    NavList::new(vec![NavSection::untitled("sections", items)])
        .selected(place_id(selected))
        .with_id("containers-sections")
        .with_accessible_name("Sections")
        .on_select(move |id| intents.push(Intent::ShowSection(Place::parse(&id))))
}

/// The sidebar: the sections, and at its foot whether the engine answers.
pub(crate) fn sidebar(
    engine: &Engine,
    selected: Place,
    connected: bool,
    intents: &Intents,
) -> Element {
    let (label, tone) = if connected {
        ("Connected", Tone::Success)
    } else {
        ("Not connected", Tone::Warning)
    };
    let status = Row::new()
        .no_wrap()
        .push(Box::new(Badge::text(label).style(ChipStyle::Tinted(tone))));
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
            .push(Box::new(section_list(engine, selected, intents)))
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
        Button::new(id, label, None)
            .with_icon(icon)
            .role(ButtonRole::Primary)
            .on_view(intents.on(intent)),
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
    row.action(icon, label, intents.on(intent))
}

/// The containers, with their state and the verbs that apply to each.
pub(crate) fn container_list(
    engine: &Engine,
    selected: Option<&str>,
    controls: bool,
    intents: &Intents,
) -> Element {
    let listed = listed_containers(engine);
    if listed.is_empty() {
        return containers_empty(engine.live, intents);
    }
    let rows: Vec<Element> = listed
        .iter()
        .map(|c| {
            let mut row = ListRow::new(format!("container-{}", c.id), c.name.clone())
                .secondary(c.image.clone())
                .selected(selected == Some(c.id.as_str()))
                .on_activate(intents.on(Intent::Select(Some(c.id.clone()))));
            row = with_status(row, c.status);
            if controls {
                for action in lifecycle(c.status) {
                    row = row_action(
                        row,
                        action_icon(action),
                        format!("{} {}", action.label(), c.name),
                        Intent::Lifecycle {
                            id: c.id.clone(),
                            action,
                        },
                        intents,
                    );
                }
            }
            Box::new(row) as Element
        })
        .collect();
    Box::new(Section::new(rows))
}

fn containers_empty(live: bool, intents: &Intents) -> Element {
    if live {
        Box::new(ActOnEmpty::new(
            EmptyState::new("No containers")
                .with_icon(Icon::Archive)
                .with_message("Create a container from an image to run it here.")
                .with_action("New container"),
            Intent::NewContainer,
            intents,
        ))
    } else {
        engine_missing()
    }
}

/// Podman did not answer: say so, once.
pub(crate) fn engine_missing() -> Element {
    Box::new(
        EmptyState::new("Podman isn't available")
            .with_icon(Icon::Archive)
            .with_message("Install Podman or start its user service to manage containers here."),
    )
}

/// The content beside the list while nothing is chosen.
pub(crate) fn nothing_selected() -> Element {
    Box::new(
        EmptyState::new("No container selected")
            .with_icon(Icon::Archive)
            .with_message("Choose a container to see its logs, usage and settings."),
    )
}

/// A stack whose children sit one `s1` step apart: a title and the line
/// that belongs to it.
struct Tight(Column);

impl Tight {
    fn new(children: Vec<Element>) -> Self {
        let mut column = Column::new().with_intrinsic_height().with_no_padding();
        for child in children {
            column = column.push(child);
        }
        Self(column)
    }
}

impl ui::widget::Decorator for Tight {
    type Inner = Column;

    fn inner(&self) -> &Column {
        &self.0
    }

    fn inner_mut(&mut self) -> &mut Column {
        &mut self.0
    }

    fn build_layout(
        &mut self,
        taffy: &mut ui::taffy::TaffyTree<()>,
        theme: &ui::theme::Theme,
        node_map: &mut std::collections::HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        self.0.set_gap(theme.tokens.spacing.s1);
        self.0.build_layout(taffy, theme, node_map)
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
    let title = Tight::new(vec![
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
    let button = Button::new(&format!("detail-{verb_id}-{}", c.id), label, None)
        .with_icon(icon)
        .with_enabled(model.controls);
    let binding = (model.live_engine && verb_id != "remove")
        .then(|| crate::published::container_resource(c, true, None))
        .and_then(|resource| ui::tilo_actions::ActionBinding::unit_for(&resource, verb_id).ok());
    match binding {
        Some(binding) => button.on_action(binding),
        None => button.on_view(intents.on(intent)),
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

fn logs(c: &Container) -> Element {
    if c.logs.is_empty() {
        return note("The container hasn't written any log output.");
    }
    Box::new(Well::new().with_background().with_child(Box::new(
        Text::new(&c.logs.join("\n")).with_role(TextRole::Code),
    )))
}

/// One measured share: its name, its reading and the bar.
fn usage(label: &str, reading: &str, fraction: f32) -> Element {
    let line = Row::new()
        .no_wrap()
        .push(Box::new(Text::new(label).with_role(TextRole::Label)))
        .push(Box::new(Spacer::new()))
        .push(Box::new(
            Text::new(reading)
                .with_role(TextRole::Caption)
                .numeric()
                .dimmed(),
        ));
    Box::new(Tight::new(vec![
        Box::new(line),
        Box::new(ProgressBar::new(fraction.clamp(0.0, 1.0)).with_label(label)),
    ]))
}

fn stats(c: &Container) -> Element {
    if !c.status.is_running() {
        return note("Usage appears while the container runs.");
    }
    let s = &c.stats;
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
        Box::new(StatTile::new("Received", ui::format::bytes(s.net_rx_bytes))),
        Box::new(StatTile::new("Sent", ui::format::bytes(s.net_tx_bytes))),
        Box::new(StatTile::new("Processes", s.pids.to_string())),
    ];
    let io = PropertyList::new(vec![
        Property::new("Disk reads", ui::format::bytes(s.block_read_bytes))
            .with_kind(TextKind::Numeric),
        Property::new("Disk writes", ui::format::bytes(s.block_write_bytes))
            .with_kind(TextKind::Numeric),
    ]);
    Box::new(
        Column::new()
            .with_intrinsic_height()
            .with_no_padding()
            .push(Box::new(Section::titled(
                "Usage",
                vec![
                    usage("CPU", &cpu, s.cpu_percent),
                    usage("Memory", &memory, s.mem_fraction()),
                ],
            )))
            .push(Box::new(Section::titled(
                "Activity",
                vec![Box::new(StatGroup::new(tiles)), Box::new(io)],
            ))),
    )
}

fn inspect(c: &Container) -> Element {
    let mut page = Column::new().with_intrinsic_height().with_no_padding();
    page = page.push(Box::new(Section::titled(
        "Details",
        vec![Box::new(PropertyList::new(vec![
            Property::new("ID", c.short_id()).with_kind(TextKind::Identifier),
            Property::new("Command", c.command.clone()).with_kind(TextKind::Code),
            Property::new("Created", c.created.clone()),
            Property::new("State", c.status_detail.clone()),
        ]))],
    )));
    if !c.ports.is_empty() {
        page = page.push(Box::new(Section::titled(
            "Ports",
            vec![Box::new(PropertyList::new(
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
            ))],
        )));
    }
    if !c.mounts.is_empty() {
        page = page.push(Box::new(Section::titled(
            "Mounts",
            vec![Box::new(PropertyList::new(
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
            ))],
        )));
    }
    if !c.env.is_empty() {
        page = page.push(Box::new(Section::titled(
            "Environment",
            vec![Box::new(PropertyList::new(
                c.env
                    .iter()
                    .map(|(k, v)| Property::new(k.clone(), v.clone()).with_kind(TextKind::Code))
                    .collect(),
            ))],
        )));
    }
    if !c.labels.is_empty() {
        page = page.push(Box::new(Section::titled(
            "Labels",
            vec![Box::new(PropertyList::new(
                c.labels
                    .iter()
                    .map(|(k, v)| {
                        Property::new(k.clone(), v.clone()).with_kind(TextKind::Identifier)
                    })
                    .collect(),
            ))],
        )));
    }
    Box::new(page)
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

/// One page of a section: a sentence about the whole, then its list.
fn page(summary: Option<String>, rows: Vec<Element>) -> Element {
    let mut column = Column::new().with_intrinsic_height();
    if let Some(summary) = summary {
        column = column.push(Box::new(
            Text::new(&summary).with_role(TextRole::Body).dimmed(),
        ));
    }
    Box::new(column.push(Box::new(Section::new(rows))))
}

pub(crate) fn images_page(engine: &Engine, controls: bool, intents: &Intents) -> Element {
    if engine.images.is_empty() {
        if !engine.live && !controls {
            return engine_missing();
        }
        return Box::new(ActOnEmpty::new(
            EmptyState::new("No images")
                .with_icon(Icon::Layers)
                .with_message("Pull an image to create containers from it.")
                .with_action("Pull image"),
            Intent::PullImage(None),
            intents,
        ));
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
            return engine_missing();
        }
        return Box::new(ActOnEmpty::new(
            EmptyState::new("No volumes")
                .with_icon(Icon::HardDrive)
                .with_message("A volume keeps a container's data when the container is removed.")
                .with_action("New volume"),
            Intent::NewVolume,
            intents,
        ));
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
            return engine_missing();
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
                .secondary(b.image.clone());
            row = with_status(row, b.status);
            if controls {
                row = row_action(
                    row,
                    Icon::Terminal,
                    format!("Enter {}", b.name),
                    Intent::EnterDistrobox(b.name.clone()),
                    intents,
                )
                .on_activate(intents.on(Intent::EnterDistrobox(b.name.clone())));
            }
            Box::new(row) as Element
        })
        .collect();
    page(None, rows)
}

// ---- empty states with an action ------------------------------------------

/// An empty state whose one action becomes an [`Intent`].
pub(crate) struct ActOnEmpty {
    state: EmptyState,
    intent: Intent,
    intents: Intents,
}

impl ActOnEmpty {
    fn new(state: EmptyState, intent: Intent, intents: &Intents) -> Self {
        Self {
            state,
            intent,
            intents: intents.clone(),
        }
    }

    fn forward(&mut self) {
        if self.state.take_action() {
            self.intents.push(self.intent.clone());
        }
    }
}

impl ui::widget::Decorator for ActOnEmpty {
    type Inner = EmptyState;

    fn inner(&self) -> &EmptyState {
        &self.state
    }

    fn inner_mut(&mut self) -> &mut EmptyState {
        &mut self.state
    }

    /// The state answers for its node, so its action reaches the queue
    /// however the host routes to it.
    fn build_layout(
        &mut self,
        taffy: &mut ui::taffy::TaffyTree<()>,
        theme: &ui::theme::Theme,
        node_map: &mut std::collections::HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        let node = self.state.build_layout(taffy, theme, node_map);
        ui::widget::decorator::register_node(node_map, node, self);
        node
    }

    fn handle_event(
        &mut self,
        event: &ui::winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        Widget::handle_event(&mut self.state, event, ctx);
        self.forward();
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::AppModel,
        path: &ui::widget::ModelPath,
    ) -> bool {
        let handled = Widget::perform_semantic_action(&mut self.state, target, action, state, path);
        self.forward();
        handled
    }

    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        state: &mut ui::widget::AppModel,
        path: &ui::widget::ModelPath,
    ) -> bool {
        let handled = Widget::perform_action(&mut self.state, action, state, path);
        self.forward();
        handled
    }
}

// ---- pull progress ---------------------------------------------------------

/// The body of the pull sheet: the state of the pull and its latest output.
pub(crate) fn pull_body(pull: &crate::app::PullProgress) -> Column {
    let bar = if pull.done || pull.failed.is_some() {
        ProgressBar::new(1.0)
    } else {
        ProgressBar::indeterminate()
    }
    .with_label("Image pull");
    let state = match &pull.failed {
        Some(error) => format!("The pull failed: {error}"),
        None if pull.done => "Finished.".to_owned(),
        None => "Downloading layers.".to_owned(),
    };
    let tail: Vec<&str> = pull
        .lines
        .iter()
        .rev()
        .take(8)
        .rev()
        .map(String::as_str)
        .collect();
    Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .push(Box::new(bar))
        .push(Box::new(
            Text::new(&state).with_role(TextRole::Body).dimmed(),
        ))
        .push(Box::new(Well::new().with_background().with_child(
            Box::new(Text::new(&tail.join("\n")).with_role(TextRole::Code)),
        )))
}

// ---- the window ------------------------------------------------------------

/// What the window shows, published by the app whenever the engine or the
/// view changes. Each part of the window rebuilds only when the slice of
/// this it shows changed.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent facts about the engine and the window, not a state machine"
)]
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
    pub history_in_header: bool,
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
}

type ContainerRowKey = Vec<(String, String, ContainerStatus)>;

/// The window: a shell per section, its columns rebuilt from the model.
pub(crate) fn window(parts: &Parts) -> ModelView<(Place, bool, bool)> {
    let p = parts.clone();
    ModelView::from_state_by(
        &parts.model,
        |m: &Model| (m.view.section, m.controls, m.history_in_header),
        move |m: &Model| -> Element { Box::new(window_shell(m, &p)) },
    )
}

fn window_shell(m: &Model, parts: &Parts) -> ui::widget::NavShell {
    let section = m.view.section;
    let intents = &parts.intents;
    let mut shell = ui::widget::NavShell::new()
        .with_id("containers-shell")
        .with_pane_state(&parts.pane)
        .sidebar(sidebar_view(parts));
    let history: Vec<Element> = if m.history_in_header {
        vec![Box::new(ui::widget::history_button())]
    } else {
        Vec::new()
    };
    let mut actions = vec![search_button()];
    let main = match section {
        Place::Containers => Some((
            "containers-new",
            "New container",
            Icon::Plus,
            Intent::NewContainer,
        )),
        Place::Images => Some((
            "images-pull",
            "Pull image",
            Icon::Download,
            Intent::PullImage(None),
        )),
        Place::Volumes => Some(("volumes-new", "New volume", Icon::Plus, Intent::NewVolume)),
        Place::Distrobox => None,
    };
    if let Some((id, label, icon, intent)) = main
        && m.controls
    {
        actions.push(primary(id, label, icon, intent, intents));
    }
    if section == Place::Containers {
        shell = shell
            .overview(list_view(parts))
            .content(detail_view(parts))
            .overview_header(section.label(), actions)
            .content_header("", history);
    } else {
        actions.extend(history);
        shell = shell
            .content(page_view(parts))
            .content_header(section.label(), actions);
    }
    shell
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
    let view = Box::new(ModelView::from_state_by(
        &parts.model,
        |m: &Model| -> (ContainerRowKey, Option<String>, bool, bool) {
            let rows = listed_containers(&m.engine)
                .into_iter()
                .map(|c| (c.id.clone(), c.name.clone(), c.status))
                .collect();
            (rows, m.view.selected.clone(), m.controls, m.missing)
        },
        move |m: &Model| {
            if m.missing {
                return engine_missing();
            }
            container_list(&m.engine, m.view.selected.as_deref(), m.controls, &intents)
        },
    ));
    scrolling(view, &parts.offsets.list)
}

fn detail_view(parts: &Parts) -> Element {
    let intents = parts.intents.clone();
    let view = Box::new(ModelView::from_state_by(
        &parts.model,
        |m: &Model| -> (Option<Container>, DetailTab, bool, bool) {
            let chosen = m
                .view
                .selected
                .as_deref()
                .and_then(|id| m.engine.find(id))
                .cloned();
            (chosen, m.view.tab, m.controls, m.missing)
        },
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

/// What a section page lists.
#[derive(PartialEq)]
enum PageKey {
    Images(Vec<Image>),
    Volumes(Vec<Volume>),
    Boxes(Vec<DistroBox>),
}

fn page_key(m: &Model) -> PageKey {
    match m.view.section {
        Place::Images => PageKey::Images(m.engine.images.clone()),
        Place::Volumes => PageKey::Volumes(m.engine.volumes.clone()),
        _ => PageKey::Boxes(m.engine.distroboxes.clone()),
    }
}

fn page_view(parts: &Parts) -> Element {
    let intents = parts.intents.clone();
    let view = Box::new(ModelView::from_state_by(
        &parts.model,
        |m: &Model| (page_key(m), m.controls, m.missing),
        move |m: &Model| {
            if m.missing {
                return engine_missing();
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
