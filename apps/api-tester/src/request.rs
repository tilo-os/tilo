//! The request builder: the URL bar, the variable hint, the tabs and what each
//! tab edits.
//!
//! Every field of the request in the builder is a field of the shared
//! record: the method, the URL and the body, the rows, the authentication.
//! Their controls submit field edits at their paths; the URL and the body
//! are text controls whose every keystroke is a splice there, so they keep
//! their focus however the rest of the view is rebuilt. Credentials stay on
//! the device: their fields set them through an effect.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use ui::icons::Icon;
use ui::tokens::TextRole;
use ui::widget::toggle_group::Segment;
use ui::widget::{
    Button, Column, Container, Dropdown, Element, Field, Row, Scrollable, Text, TextInput,
    TextInputKind, ToggleGroup,
};

use crate::commands::{AuthField, Cmd, FormMode, FormTarget, Sink, Table};
use crate::kv::{self, KvQueue};
use crate::model::{Auth, BodyMode, HttpMethod, KeyValue, Request, RequestTab};
use crate::resource::Workspace;
use crate::vars;
use ui::widget::{Slot, SlotHandle};

/// Ids of the blank row each table ends with.
#[derive(Clone, Debug)]
pub(crate) struct BlankRows {
    pub(crate) params: String,
    pub(crate) headers: String,
    pub(crate) form: String,
}

impl BlankRows {
    pub(crate) fn new() -> Self {
        let fresh = || uuid::Uuid::new_v4().to_string();
        Self {
            params: fresh(),
            headers: fresh(),
            form: fresh(),
        }
    }

    pub(crate) fn of(&self, table: Table) -> &str {
        match table {
            Table::Params => &self.params,
            Table::Headers => &self.headers,
            Table::Form => &self.form,
        }
    }

    pub(crate) fn renew(&mut self, table: Table) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        match table {
            Table::Params => self.params.clone_from(&id),
            Table::Headers => self.headers.clone_from(&id),
            Table::Form => self.form.clone_from(&id),
        }
        id
    }
}

/// A tab's panel and what it reports besides its element.
pub(crate) struct Panel {
    pub(crate) element: Element,
    /// The slot inside the panel that its own mode switch replaces.
    pub(crate) inner: Option<SlotHandle>,
    /// The key/value table showing, and the queue its rows change through.
    pub(crate) table: Option<(Table, KvQueue)>,
}

/// The tab row and the panel it chooses, built together.
pub(crate) struct Built {
    pub(crate) element: Element,
    /// The slot the tab's panel sits in.
    pub(crate) slot: SlotHandle,
    /// The slot inside the first panel, and its table.
    pub(crate) inner: Option<SlotHandle>,
    pub(crate) table: Option<(Table, KvQueue)>,
}

fn pusher(sink: &Sink, cmd: Cmd) -> impl FnMut() + 'static {
    let sink = sink.clone();
    move || sink.borrow_mut().push(cmd.clone())
}

fn body(text: &str) -> Element {
    Box::new(Text::new(text).with_role(TextRole::Body).dimmed())
}

// ---------------------------------------------------------------------------
// URL bar
// ---------------------------------------------------------------------------

/// The method and the URL on one row.
pub(crate) fn url_bar(sink: &Sink) -> Element {
    let method = Dropdown::from_state(
        HttpMethod::ALL
            .iter()
            .map(|m| m.as_str().to_owned())
            .collect(),
        &sink.method,
    )
    .with_id("api.method")
    .with_accessible_name("Method");
    let field = Workspace::paths().request.method;
    let method = sink.bind_field(method, field.clone().cast::<String>(), move |_, method| {
        field.set(HttpMethod::from_str(&method))
    });
    let url = request_text(
        TextInput::local()
            .with_kind(TextInputKind::Url)
            .with_id("api.url")
            .with_placeholder("Enter a URL")
            .with_accessible_name("Request URL"),
        sink,
        &sink.url.borrow(),
        Workspace::paths().request.url,
    );
    Box::new(
        Row::new().push(Box::new(method)).push(Box::new(
            Container::new()
                .with_no_padding()
                .with_flex_grow()
                .with_child(url),
        )),
    )
}

/// Save and Send: the request's actions, which the window's bar shows.
/// Save and Send — or Cancel, while a request is on its way, so a server
/// that never answers is never a locked app.
pub(crate) fn request_actions(sink: &Sink, sending: bool) -> Element {
    let save = Button::icon_only("api.save-request", Icon::Check, "Save request")
        .tooltip("Save this request to a collection")
        // It opens the save form; the save it leads to is its own action.
        .on_own_device_effect(pusher(
            sink,
            Cmd::OpenForm(FormMode::SaveRequest, FormTarget::default()),
        ));
    let action = if sending {
        sink.effect(
            Button::icon_only("api.cancel-send", Icon::X, "Cancel request")
                .tooltip("Stop waiting for the server"),
            &crate::actions::ApiTesterEffect::CancelSend,
        )
    } else {
        sink.send(Button::icon_only("api.send", Icon::Send, "Send"))
    };
    Box::new(Row::new().push(Box::new(save)).push(Box::new(action)))
}

/// What the `{{variables}}` in the URL resolve to, as one quiet line. Empty
/// when the URL has none.
pub(crate) fn variable_hint(
    env: Rc<RefCell<HashMap<String, String>>>,
    url: Rc<RefCell<String>>,
) -> Element {
    Box::new(
        Text::dynamic(move || {
            let refs = vars::referenced_vars(&url.borrow());
            if refs.is_empty() {
                return String::new();
            }
            let env = env.borrow();
            let unresolved: Vec<&String> = refs
                .iter()
                .filter(|name| !env.contains_key(*name))
                .collect();
            if unresolved.is_empty() {
                ui::i18n::I18nManager::format(
                    "api-variables-resolved",
                    &[("count", refs.len().into())],
                )
            } else {
                ui::i18n::I18nManager::format(
                    "api-variables-unresolved",
                    &[
                        ("count", unresolved.len().into()),
                        (
                            "names",
                            unresolved
                                .iter()
                                .map(|name| name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                                .into(),
                        ),
                    ],
                )
            }
        })
        .with_role(TextRole::Caption)
        .dimmed(),
    )
}

// ---------------------------------------------------------------------------
// Tabs and panels
// ---------------------------------------------------------------------------

/// The tab row and the panel it chooses.
pub(crate) fn section(
    request: &Request,
    tab: RequestTab,
    blanks: &BlankRows,
    sink: &Sink,
) -> Built {
    let tab_sink = sink.clone();
    let tabs = ui::widget::ViewTabs::new(
        RequestTab::ALL
            .iter()
            .map(|t| Segment::new(t.label()))
            .collect(),
        RequestTab::ALL.iter().position(|t| *t == tab).unwrap_or(0),
    )
    .on_view("api.request-tabs", move |i| {
        tab_sink.borrow_mut().push(Cmd::SetTab(i));
    });
    let first = panel(request, tab, blanks, sink);
    let (slot, handle) = Slot::new(first.element);
    let column = Column::new()
        .with_no_padding()
        .with_rhythm(ui::tokens::Rhythm::Related)
        .with_intrinsic_height()
        .push(Box::new(tabs))
        .push(Box::new(slot));
    Built {
        element: Box::new(column),
        slot: handle,
        inner: first.inner,
        table: first.table,
    }
}

/// What `tab` edits.
pub(crate) fn panel(request: &Request, tab: RequestTab, blanks: &BlankRows, sink: &Sink) -> Panel {
    match tab {
        RequestTab::Params => table_panel(
            Table::Params,
            "Query parameters",
            &request.params,
            blanks,
            sink,
        ),
        RequestTab::Headers => {
            table_panel(Table::Headers, "Headers", &request.headers, blanks, sink)
        }
        RequestTab::Auth => auth_panel(request, sink),
        RequestTab::Body => body_panel(request, blanks, sink),
    }
}

fn scrolling(content: Element) -> Element {
    Box::new(
        Scrollable::new_vertical()
            .fit_content_height()
            .with_child(content),
    )
}

fn table_panel(
    table: Table,
    label: &str,
    rows: &[KeyValue],
    blanks: &BlankRows,
    sink: &Sink,
) -> Panel {
    let built = kv::build(table, label, rows, blanks.of(table), sink);
    Panel {
        element: scrolling(built.element),
        inner: None,
        table: Some((table, built.queue)),
    }
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

fn auth_kind_index(auth: &Auth) -> usize {
    Auth::LABELS
        .iter()
        .position(|label| *label == auth.label())
        .unwrap_or(0)
}

fn auth_input(sink: &Sink, which: AuthField, value: &str, secret: bool, name: &str) -> TextInput {
    let input = TextInput::local()
        .with_kind(if secret {
            TextInputKind::Secret
        } else {
            TextInputKind::Code
        })
        .with_initial_value(value)
        .with_accessible_name(name);
    let auth = Workspace::paths().request.auth;
    let input = match sink.actions.borrow().as_ref() {
        Some(actions) if secret => {
            actions.bind_effect(input, crate::actions::ApiTesterEffect::SET_CREDENTIAL)
        }
        Some(_) => {
            let field = match which {
                AuthField::Username => auth.username,
                AuthField::Header => auth.header,
                _ => return input.disabled(true),
            };
            sink.bind_field(input, field, ui::tilo_resource::Path::replace)
        }
        None => input.disabled(true),
    };
    if secret {
        input.with_secret(true)
    } else {
        input
    }
}

fn auth_fields(auth: &Auth, sink: &Sink) -> Element {
    let column = Column::new().with_no_padding().with_intrinsic_height();
    match auth {
        Auth::SecretRef { .. } => Box::new(column.push(body("Stored authentication"))),
        Auth::None => Box::new(column.push(body("This request does not use authentication."))),
        Auth::Bearer { token } => Box::new(column.push(Box::new(Field::new(
            "Token",
            auth_input(sink, AuthField::Token, token, true, "Token"),
        )))),
        Auth::Basic { username, password } => Box::new(
            column
                .push(Box::new(Field::new(
                    "Username",
                    auth_input(sink, AuthField::Username, username, false, "Username"),
                )))
                .push(Box::new(Field::new(
                    "Password",
                    auth_input(sink, AuthField::Password, password, true, "Password"),
                ))),
        ),
        Auth::ApiKey { header, value } => Box::new(
            column
                .push(Box::new(Field::new(
                    "Header name",
                    auth_input(sink, AuthField::Header, header, false, "Header name"),
                )))
                .push(Box::new(Field::new(
                    "Value",
                    auth_input(sink, AuthField::Value, value, true, "Value"),
                ))),
        ),
    }
}

/// The scheme and the fields it needs.
pub(crate) fn auth_inner(auth: &Auth, sink: &Sink) -> Element {
    auth_fields(auth, sink)
}

fn auth_panel(request: &Request, sink: &Sink) -> Panel {
    let kind_sink = sink.clone();
    let kind = ToggleGroup::new(
        Auth::LABELS
            .iter()
            .map(|label| Segment::new(*label))
            .collect(),
        auth_kind_index(&request.auth),
    )
    .on_view("api.auth-kind", move |i| {
        kind_sink.borrow_mut().push(Cmd::SetAuthKind(i));
    });
    let (slot, handle) = Slot::new(auth_fields(&request.auth, sink));
    let column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(Box::new(kind))
        .push(Box::new(slot));
    Panel {
        element: scrolling(Box::new(column)),
        inner: Some(handle),
        table: None,
    }
}

// ---------------------------------------------------------------------------
// Body
// ---------------------------------------------------------------------------

/// What the chosen body mode edits.
pub(crate) fn body_inner(
    request: &Request,
    blanks: &BlankRows,
    sink: &Sink,
) -> (Element, Option<(Table, KvQueue)>) {
    match request.body_mode {
        BodyMode::None => (body("This request does not have a body."), None),
        BodyMode::Json | BodyMode::Raw => (
            request_text(
                TextInput::local()
                    .multiline(true)
                    .with_mono()
                    .with_id("api.body")
                    .with_accessible_name("Request body"),
                sink,
                &request.body_text,
                Workspace::paths().request.body,
            ),
            None,
        ),
        BodyMode::Form => {
            let built = kv::build(
                Table::Form,
                "Form fields",
                &request.form,
                blanks.of(Table::Form),
                sink,
            );
            (built.element, Some((Table::Form, built.queue)))
        }
    }
}

fn body_panel(request: &Request, blanks: &BlankRows, sink: &Sink) -> Panel {
    let mode_sink = sink.clone();
    let modes = ToggleGroup::new(
        BodyMode::ALL
            .iter()
            .map(|mode| Segment::new(mode.label()))
            .collect(),
        BodyMode::ALL
            .iter()
            .position(|mode| *mode == request.body_mode)
            .unwrap_or(0),
    )
    .on_view("api.body-mode", move |i| {
        mode_sink.borrow_mut().push(Cmd::SetBodyMode(i));
    });
    let (inner, table) = body_inner(request, blanks, sink);
    let (slot, handle) = Slot::new(inner);
    let column = Column::new()
        .with_no_padding()
        .push(Box::new(modes))
        .push(Box::new(slot));
    Panel {
        element: scrolling(Box::new(column)),
        inner: Some(handle),
        table,
    }
}

/// `input` over the builder's text at `path`, every edit a splice there
/// through the workspace's dispatcher. Before the framework attaches the
/// window it only shows `shown`; the app builds it again once attached.
fn request_text(
    input: TextInput,
    sink: &Sink,
    shown: &str,
    path: ui::tilo_resource::Path<Workspace, String>,
) -> Element {
    match sink.dispatcher.borrow().as_ref() {
        Some(dispatcher) => Box::new(ui::framework::TextControl::at(
            input,
            dispatcher.clone(),
            path,
        )),
        None => Box::new(input.with_initial_value(shown).disabled(true)),
    }
}
