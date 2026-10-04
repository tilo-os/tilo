//! The window's retained views, built from the app's state: the navigation
//! rail, each screen's header actions, the connection list, the connection
//! form and the session states.
//!
//! Views hold no state of their own. Every control pushes an [`Action`] into
//! the shared [`Requests`] queue; the app drains it after routing each event
//! and rebuilds what changed.

use std::cell::RefCell;
use std::rc::Rc;

use ui::LocalState;
use ui::icons::Icon;
use ui::search::{SearchRequest, search_button};
use ui::theme::Theme;
use ui::widget::toggle_group::Segment;
use ui::widget::{
    Actions, Banner, Button, ButtonRole, Column, Element, EmptyState, Field, Form, FormControl,
    FormPage, FormSection, ListRow, NavItem, NavList, NavSection, Scrollable, Section,
    SectionHeader, TextInput, TextInputKind, ToggleGroup, Widget,
};

use crate::app::Action;
use crate::model::{AuthMethod, ConnectionId, ConnectionKind};

/// The queue every control reports to.
pub(crate) type Requests = Rc<RefCell<Vec<Action>>>;

/// A callback that queues `action`.
fn queue(requests: &Requests, action: Action) -> impl FnMut() + 'static {
    let requests = Rc::clone(requests);
    move || requests.borrow_mut().push(action)
}

/// Where the rail points.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum Destination {
    /// Every saved connection.
    #[default]
    Connections,
    /// The connections pinned to the top of the list.
    Pinned,
    /// The open session.
    Session,
}

impl Destination {
    fn id(self) -> &'static str {
        match self {
            Self::Connections => "connections",
            Self::Pinned => "pinned",
            Self::Session => "session",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        match id {
            "connections" => Some(Self::Connections),
            "pinned" => Some(Self::Pinned),
            "session" => Some(Self::Session),
            _ => None,
        }
    }
}

/// The navigation rail: every connection, the pinned ones and, while one is
/// open, the session.
pub(crate) fn rail(
    requests: &Requests,
    selected: Destination,
    session: Option<ConnectionKind>,
) -> NavList {
    let mut items = vec![
        NavItem::new(Destination::Connections.id(), "All").icon(Icon::List),
        NavItem::new(Destination::Pinned.id(), "Pinned").icon(Icon::Pin),
    ];
    if let Some(kind) = session {
        items.push(NavItem::new(Destination::Session.id(), "Session").icon(
            if kind.is_terminal() {
                Icon::Terminal
            } else {
                Icon::Monitor
            },
        ));
    }
    let requests = Rc::clone(requests);
    NavList::new(vec![NavSection::untitled("destinations", items)])
        .rail()
        .with_id("remote.rail")
        .with_accessible_name("Remote")
        .selected(selected.id())
        .on_select(move |id| {
            if let Some(destination) = Destination::from_id(&id) {
                requests.borrow_mut().push(Action::Show(destination));
            }
        })
}

/// The list's header actions: search, and a new connection once there is a
/// list to add to (an empty list offers it in its empty state).
pub(crate) fn list_actions(requests: &Requests, has_connections: bool) -> Vec<Element> {
    let mut actions: Vec<Element> = vec![Box::new(search_button(SearchRequest::this_app()))];
    if has_connections {
        actions.push(Box::new(
            Button::new("remote.connection.new", "New connection", None)
                .with_icon(Icon::Plus)
                .on_click(queue(requests, Action::NewConnection)),
        ));
    }
    actions
}

/// What one row of the connection list shows.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RowModel {
    pub id: ConnectionId,
    pub name: String,
    pub kind: ConnectionKind,
    /// Where the machine is: the address a network protocol dials, or that
    /// it is a Tilo device (whose endpoint id means nothing to read).
    pub address: String,
    /// The last session, or why Remote can't open one here.
    pub meta: String,
    pub presence: crate::model::Presence,
    pub favourite: bool,
    pub selected: bool,
}

/// What a connection opens: a terminal or a screen.
fn kind_icon(kind: ConnectionKind) -> Icon {
    if kind.is_terminal() {
        Icon::Terminal
    } else {
        Icon::Monitor
    }
}

fn connection_row(requests: &Requests, row: &RowModel) -> ListRow {
    let list_row = ListRow::new(format!("remote.connection:{}", row.id), &row.name)
        .icon(kind_icon(row.kind))
        .secondary(&row.address)
        .meta(&row.meta)
        .selected(row.selected)
        .on_activate(queue(requests, Action::RequestConnect(row.id)))
        .action(
            Icon::Pencil,
            "Edit connection",
            queue(requests, Action::EditConnection(row.id)),
        )
        .action(
            Icon::Pin,
            if row.favourite {
                "Unpin connection"
            } else {
                "Pin connection"
            },
            queue(requests, Action::ToggleFavorite(row.id)),
        );
    // Every row carries a dot, so the meta column keeps one trailing edge:
    // answered last time, or not (yet) seen answering.
    match row.presence {
        crate::model::Presence::Online => list_row.active(),
        crate::model::Presence::Offline | crate::model::Presence::Unknown => list_row.idle(),
    }
}

/// A vertical page inside the content gutter, groups `rhythm.group` apart.
fn page(theme: &Theme, children: Vec<Element>) -> Element {
    let mut column = Column::new()
        .with_intrinsic_height()
        .with_gap(theme.tokens.spacing.rhythm.group);
    for child in children {
        column = column.push(child);
    }
    Box::new(Scrollable::new_vertical().with_child(Box::new(column)))
}

/// The connection list for `destination`: the pinned connections first, as
/// their own group, then the rest.
pub(crate) fn connection_list(
    theme: &Theme,
    requests: &Requests,
    destination: Destination,
    rows: &[RowModel],
    has_connections: bool,
) -> Element {
    if rows.is_empty() {
        return if has_connections {
            Box::new(
                EmptyState::new("No pinned connections")
                    .with_icon(Icon::Pin)
                    .with_message("Pin the machines you use most to keep them here."),
            )
        } else {
            Box::new(Actionable::new(
                EmptyState::new("No saved connections")
                    .with_icon(Icon::Monitor)
                    .with_message("Save a machine to reach it over SSH, VNC or as a Tilo device.")
                    .with_action("New connection")
                    .with_action_id("remote.connection.new"),
                requests,
                Action::NewConnection,
            ))
        };
    }
    let list = |rows: Vec<&RowModel>| -> Vec<Element> {
        rows.into_iter()
            .map(|row| Box::new(connection_row(requests, row)) as Element)
            .collect()
    };
    let (pinned, others): (Vec<&RowModel>, Vec<&RowModel>) =
        rows.iter().partition(|row| row.favourite);
    let sections: Vec<Element> =
        if destination == Destination::Pinned || pinned.is_empty() || others.is_empty() {
            vec![Box::new(Section::new(list(rows.iter().collect())))]
        } else {
            vec![
                Box::new(Section::new(list(pinned)).with_header(SectionHeader::group("Pinned"))),
                Box::new(
                    Section::new(list(others))
                        .with_header(SectionHeader::group("Other connections")),
                ),
            ]
        };
    page(theme, sections)
}

/// The text behind each form field, kept across rebuilds of the form.
#[derive(Clone)]
pub(crate) struct FormFields {
    pub name: LocalState<String>,
    pub host: LocalState<String>,
    pub port: LocalState<String>,
    pub user: LocalState<String>,
    pub group: LocalState<String>,
}

impl Default for FormFields {
    fn default() -> Self {
        Self {
            name: LocalState::new(String::new()),
            host: LocalState::new(String::new()),
            port: LocalState::new(String::new()),
            user: LocalState::new(String::new()),
            group: LocalState::new(String::new()),
        }
    }
}

/// A discovered Tilo device as the form's picker shows it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct DeviceModel {
    pub name: String,
    pub online: bool,
    pub picked: bool,
}

/// The problems Save found, by the field that repairs each.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct FormProblems {
    pub name: Option<String>,
    pub host: Option<String>,
    pub port: Option<String>,
    /// Problems no single field repairs (an authentication the protocol
    /// does not offer).
    pub other: Vec<String>,
}

/// What the form shows.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FormModel {
    pub editing: Option<ConnectionId>,
    pub kind: ConnectionKind,
    pub auth: AuthMethod,
    pub devices: Vec<DeviceModel>,
    pub problems: FormProblems,
}

fn text_field(state: &LocalState<String>, id: &str, placeholder: &str) -> TextInput {
    TextInput::from_state(state)
        .with_id(id)
        .with_placeholder(placeholder)
}

fn with_problem(field: Field, problem: Option<&String>) -> Field {
    match problem {
        Some(problem) => field.with_error(problem),
        None => field,
    }
}

/// A segmented choice named by the form field that labels it, so assistive
/// technology announces "Protocol" with the chosen segment.
struct Choice {
    inner: ToggleGroup,
    name: Option<String>,
}

impl Choice {
    fn new(inner: ToggleGroup) -> Self {
        Self { inner, name: None }
    }
}

impl ui::widget::Decorator for Choice {
    type Inner = ToggleGroup;

    fn inner(&self) -> &ToggleGroup {
        &self.inner
    }

    fn inner_mut(&mut self) -> &mut ToggleGroup {
        &mut self.inner
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let node = self.inner.accessibility()?;
        Some(match &self.name {
            Some(name) => node.with_name(name.clone()),
            None => node,
        })
    }
}

impl FormControl for Choice {
    fn with_form_accessible_name(mut self, name: &str) -> Self {
        self.name = Some(name.to_owned());
        self
    }
}

/// The protocols the form offers. A saved legacy RDP profile keeps its own
/// segment so the choice never hides the protocol the profile uses.
fn offered_kinds(current: ConnectionKind) -> Vec<ConnectionKind> {
    let mut kinds = ConnectionKind::available_to_create().to_vec();
    if !kinds.contains(&current) {
        kinds.insert(0, current);
    }
    kinds
}

/// The protocol choice.
fn protocol_field(requests: &Requests, model: &FormModel) -> Field {
    let kinds = offered_kinds(model.kind);
    let selected = kinds.iter().position(|k| *k == model.kind).unwrap_or(0);
    let segments = kinds.iter().map(|k| Segment::new(k.label())).collect();
    let queue_to = Rc::clone(requests);
    let protocol = ToggleGroup::new(segments, selected)
        .with_id("remote.form.protocol")
        .on_change(move |index| {
            if let Some(kind) = kinds.get(index) {
                queue_to.borrow_mut().push(Action::SetKind(*kind));
            }
        });
    Field::new("Protocol", Choice::new(protocol)).with_trailing_control()
}

/// The Tilo devices nearby, to fill the form from.
fn device_list(requests: &Requests, devices: &[DeviceModel]) -> Element {
    let rows: Vec<Element> = devices
        .iter()
        .enumerate()
        .map(|(index, device)| {
            let row = ListRow::new(format!("remote.device:{index}"), &device.name)
                .icon(Icon::Monitor)
                .selected(device.picked)
                .on_activate(queue(requests, Action::PickTiloDevice(index)));
            Box::new(if device.online {
                row.active()
            } else {
                row.idle()
            }) as Element
        })
        .collect();
    Box::new(Section::new(rows).with_header(SectionHeader::group("Nearby devices")))
}

/// Which machine, where it is, who signs in and how, and how it is filed.
/// One section, so every label shares one column.
fn machine_section(requests: &Requests, fields: &FormFields, model: &FormModel) -> FormSection {
    let tilo = model.kind == ConnectionKind::Tilo;
    let mut section = FormSection::new("Machine").field(protocol_field(requests, model));
    if model.kind == ConnectionKind::Rdp {
        section = section.content(Box::new(
            Banner::warning("Remote can't open RDP connections")
                .with_message("Choose SSH or VNC to keep using this machine."),
        ));
    }
    for problem in &model.problems.other {
        section = section.content(Box::new(Banner::error(problem.clone())));
    }
    if tilo && !model.devices.is_empty() {
        section = section.content(device_list(requests, &model.devices));
    }
    section = section.field(with_problem(
        Field::new(
            "Name",
            text_field(&fields.name, "remote.field:name", "Home server"),
        ),
        model.problems.name.as_ref(),
    ));
    if tilo {
        return section.field(with_problem(
            Field::new(
                "Device",
                text_field(&fields.host, "remote.field:host", "Device endpoint id")
                    .with_kind(TextInputKind::Code),
            ),
            model.problems.host.as_ref(),
        ));
    }
    section = section
        .field(with_problem(
            Field::new(
                "Host",
                text_field(&fields.host, "remote.field:host", "Host name or IP address")
                    .with_kind(TextInputKind::Url),
            ),
            model.problems.host.as_ref(),
        ))
        .field(with_problem(
            Field::new(
                "Port",
                text_field(&fields.port, "remote.field:port", "Port")
                    .with_kind(TextInputKind::Number)
                    .with_char_filter(|c| c.is_ascii_digit()),
            ),
            model.problems.port.as_ref(),
        ));
    sign_in_fields(requests, fields, model, section).field(Field::new(
        "Group",
        text_field(&fields.group, "remote.field:group", "Optional"),
    ))
}

/// Who signs in and how, where the protocol asks for either.
fn sign_in_fields(
    requests: &Requests,
    fields: &FormFields,
    model: &FormModel,
    mut section: FormSection,
) -> FormSection {
    let methods = AuthMethod::available_for(model.kind);
    if matches!(model.kind, ConnectionKind::Ssh | ConnectionKind::Rdp) {
        section = section.field(Field::new(
            "User",
            text_field(&fields.user, "remote.field:user", "User name")
                .with_kind(TextInputKind::Username),
        ));
    }
    if methods.len() > 1 {
        let selected = methods.iter().position(|a| *a == model.auth).unwrap_or(0);
        let segments = methods.iter().map(|a| Segment::new(a.label())).collect();
        let queue_to = Rc::clone(requests);
        let auth = ToggleGroup::new(segments, selected)
            .with_id("remote.form.auth")
            .on_change(move |index| {
                if let Some(method) = methods.get(index) {
                    queue_to.borrow_mut().push(Action::SetAuth(*method));
                }
            });
        section =
            section.field(Field::new("Sign in with", Choice::new(auth)).with_trailing_control());
    }
    section
}

/// The add / edit form. Save and Cancel stand in the header row, so they
/// stay in view however long the form grows; deleting a saved connection is
/// the form's last, separate action.
pub(crate) fn connection_form(
    requests: &Requests,
    fields: &FormFields,
    model: &FormModel,
) -> Element {
    let mut form = Form::new().section(machine_section(requests, fields, model));
    if let Some(id) = model.editing {
        form = form.with_actions(Actions::new(
            Button::new("remote.form.delete", "Delete connection", None)
                .role(ButtonRole::Destructive)
                .on_click(queue(requests, Action::RequestDelete(id))),
        ));
    }
    let title = if model.editing.is_some() {
        "Edit connection"
    } else {
        "New connection"
    };
    Box::new(FormPage::new(title, form))
}

/// The form's header actions: leaving it, and saving the connection.
pub(crate) fn form_actions(requests: &Requests) -> Vec<Element> {
    vec![
        Box::new(
            Button::new("remote.form.cancel", "Cancel", None)
                .on_click(queue(requests, Action::Back)),
        ),
        Box::new(
            Button::new("remote.form.save", "Save", None)
                .role(ButtonRole::Primary)
                .on_click(queue(requests, Action::SaveForm)),
        ),
    ]
}

/// A session that shows no remote surface: connecting, failed, unavailable
/// or ended. One sentence says why; the one action is the way forward.
pub(crate) struct StatusModel {
    pub icon: Icon,
    pub title: String,
    pub message: String,
    pub error: bool,
    pub action: Option<(&'static str, Action)>,
}

pub(crate) fn session_status(requests: &Requests, model: StatusModel) -> Element {
    let state = if model.error {
        EmptyState::error(model.title, model.message)
    } else {
        EmptyState::new(model.title).with_message(model.message)
    }
    .with_icon(model.icon);
    match model.action {
        Some((label, action)) => Box::new(Actionable::new(
            state
                .with_action(label)
                .with_action_id("remote.session.recover"),
            requests,
            action,
        )),
        None => Box::new(state),
    }
}

/// The session's header actions while it is live: fitting and filling the
/// window with a remote desktop, and ending the session. A session that is
/// not live offers its one way forward in the content instead.
pub(crate) fn session_actions(
    requests: &Requests,
    framebuffer: bool,
    scale_to_fit: bool,
) -> Vec<Element> {
    let mut actions: Vec<Element> = Vec::new();
    if framebuffer {
        actions.push(Box::new(
            Button::new(
                "remote.session.scale",
                if scale_to_fit {
                    "Actual size"
                } else {
                    "Fit to window"
                },
                None,
            )
            .on_click(queue(requests, Action::ToggleScale)),
        ));
        actions.push(Box::new(
            Button::icon_only("remote.session.fullscreen", Icon::Maximize2, "Full screen")
                .on_click(queue(requests, Action::ToggleFullscreen)),
        ));
    }
    actions.push(Box::new(
        Button::new("remote.session.disconnect", "Disconnect", None)
            .role(ButtonRole::Destructive)
            .on_click(queue(requests, Action::Disconnect)),
    ));
    actions
}

/// An [`EmptyState`] whose one action reports to the queue.
pub(crate) struct Actionable {
    inner: EmptyState,
    requests: Requests,
    action: Action,
}

impl Actionable {
    fn new(inner: EmptyState, requests: &Requests, action: Action) -> Self {
        Self {
            inner,
            requests: Rc::clone(requests),
            action,
        }
    }

    fn report(&mut self) {
        if self.inner.take_action() {
            self.requests.borrow_mut().push(self.action);
        }
    }
}

impl ui::widget::Decorator for Actionable {
    type Inner = EmptyState;

    fn inner(&self) -> &EmptyState {
        &self.inner
    }

    fn inner_mut(&mut self) -> &mut EmptyState {
        &mut self.inner
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        self.inner.handle_event(event, ctx);
        self.report();
    }

    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        state: &mut ui::widget::AppModel,
        path: &ui::widget::ModelPath,
    ) -> bool {
        let done = self.inner.perform_action(action, state, path);
        self.report();
        done
    }
}
