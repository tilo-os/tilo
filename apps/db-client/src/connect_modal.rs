//! The dialogs the workspace presents: the new-connection form and the
//! confirmation before a statement that changes data runs.
//!
//! Both are the toolkit's [`ModalDialog`]. The form's fields are local state
//! the dialog's actions read, so the field set can change with the engine (a
//! file for `SQLite`; host, port, database, user and password for the network
//! engines) without the dialog keeping hold of any of them.

use ui::LocalState;
use ui::TextRole;
use ui::widget::toggle_group::Segment;
use ui::widget::{
    Column, Decorator, Density, Element, Field, Form, FormSection, ModalDialog, SegmentedStyle,
    Text, TextInput, TextInputKind, ToggleGroup, Widget,
};

use crate::i18n::t;
use crate::link::{Intent, Link};
use crate::model::{ConnectionProfile, Engine};

/// The engines in the order the picker lists them.
const ENGINES: [Engine; 3] = [Engine::Postgres, Engine::MySql, Engine::Sqlite];

fn engine_at(index: usize) -> Engine {
    ENGINES.get(index).copied().unwrap_or(Engine::Postgres)
}

/// What the person typed. Clones share the same values.
#[derive(Clone)]
pub struct Fields {
    engine: LocalState<usize>,
    name: LocalState<String>,
    host: LocalState<String>,
    port: LocalState<String>,
    database: LocalState<String>,
    user: LocalState<String>,
    password: LocalState<String>,
    file_path: LocalState<String>,
}

impl Fields {
    fn new() -> Self {
        let fields = Self {
            engine: LocalState::new(0),
            name: LocalState::new(String::new()),
            host: LocalState::new(String::new()),
            port: LocalState::new(String::new()),
            database: LocalState::new(String::new()),
            user: LocalState::new(String::new()),
            password: LocalState::new(String::new()),
            file_path: LocalState::new(String::new()),
        };
        fields.reset();
        fields
    }

    /// Back to a blank `PostgreSQL` connection.
    fn reset(&self) {
        let engine = Engine::Postgres;
        self.engine.set(0);
        self.name.set(String::new());
        self.host.set("localhost".to_owned());
        self.port.set(engine.default_port().to_string());
        self.database.set(String::new());
        self.user.set(String::new());
        self.password.set(String::new());
        self.file_path.set(String::new());
    }

    fn engine(&self) -> Engine {
        engine_at(self.engine.with(|index| *index))
    }

    /// Switch engine. A network engine keeps the port the person typed and
    /// otherwise takes its own usual one.
    fn choose(&self, engine: Engine) {
        let previous = self.engine();
        let index = ENGINES.iter().position(|candidate| *candidate == engine);
        self.engine.set(index.unwrap_or(0));
        if engine != Engine::Sqlite {
            let port = self.port.with(Clone::clone);
            if port.is_empty() || port == previous.default_port().to_string() {
                self.port.set(engine.default_port().to_string());
            }
        }
    }

    pub fn profile(&self) -> ConnectionProfile {
        let engine = self.engine();
        let name = self.name.with(|name| name.trim().to_owned());
        let title = if name.is_empty() {
            t("db-untitled")
        } else {
            name.clone()
        };
        let mut profile = ConnectionProfile::new(&title, engine);
        profile.host = self.host.with(Clone::clone);
        profile.port = self
            .port
            .with(|port| port.trim().parse().ok())
            .unwrap_or_else(|| engine.default_port());
        profile.database = self.database.with(Clone::clone);
        profile.user = self.user.with(Clone::clone);
        profile.password = self.password.with(Clone::clone);
        profile.file_path = self.file_path.with(Clone::clone);
        profile
    }
}

fn input(id: &str, state: &LocalState<String>) -> TextInput {
    TextInput::from_state(state).with_id(id)
}

/// The connection name, which is where typing starts.
fn name_input(fields: &Fields) -> TextInput {
    input("db.modal.name", &fields.name).with_kind(TextInputKind::Name)
}

fn network_form(fields: &Fields) -> Form {
    Form::new().with_density(Some(Density::Compact)).section(
        FormSection::new("")
            .field(Field::new(&t("db-connection-name"), name_input(fields)))
            .field(Field::new(
                &t("db-host"),
                input("db.modal.host", &fields.host).with_kind(TextInputKind::Url),
            ))
            .field(Field::new(
                &t("db-port"),
                input("db.modal.port", &fields.port)
                    .with_kind(TextInputKind::Number)
                    .with_char_filter(|c| c.is_ascii_digit()),
            ))
            .field(Field::new(
                &t("db-database"),
                input("db.modal.database", &fields.database).with_kind(TextInputKind::Username),
            ))
            .field(Field::new(
                &t("db-user"),
                input("db.modal.user", &fields.user).with_kind(TextInputKind::Username),
            ))
            .field(
                Field::new(
                    &t("db-password"),
                    input("db.modal.password", &fields.password).with_kind(TextInputKind::Password),
                )
                .with_help(&t("db-password-stays")),
            ),
    )
}

fn sqlite_form(fields: &Fields) -> Form {
    Form::new().with_density(Some(Density::Compact)).section(
        FormSection::new("")
            .field(Field::new(&t("db-connection-name"), name_input(fields)))
            .field(Field::new(
                &t("db-file-path"),
                input("db.modal.file-path", &fields.file_path).with_kind(TextInputKind::Path),
            )),
    )
}

fn content(fields: &Fields) -> Element {
    let chosen = fields.clone();
    let picker = ToggleGroup::new(
        ENGINES
            .iter()
            .map(|engine| Segment::new(engine.label()))
            .collect(),
        fields.engine.with(|index| *index),
    )
    .with_style(SegmentedStyle::Segmented)
    .fill()
    .with_id("db.modal.engine")
    .on_change(move |index| chosen.choose(engine_at(index)));
    let form: Element = match fields.engine() {
        Engine::Sqlite => Box::new(sqlite_form(fields)),
        Engine::Postgres | Engine::MySql => Box::new(network_form(fields)),
    };
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(picker))
            .push(form),
    )
}

/// The form: an engine picker above the fields that engine needs.
pub struct ConnectForm {
    fields: Fields,
    content: Element,
    built_for: Engine,
    structure_dirty: bool,
    /// Typing should start at the first field.
    focus_first: bool,
}

impl ConnectForm {
    fn new(fields: &Fields) -> Self {
        Self {
            fields: fields.clone(),
            content: content(fields),
            built_for: fields.engine(),
            structure_dirty: false,
            focus_first: true,
        }
    }
}

impl Decorator for ConnectForm {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn sync(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) {
        if self.fields.engine() != self.built_for {
            self.built_for = self.fields.engine();
            self.content = content(&self.fields);
            self.structure_dirty = true;
            self.focus_first = true;
        }
        self.content.sync(state, path);
    }

    fn take_structure_dirty(&mut self) -> bool {
        let rebuilt = std::mem::take(&mut self.structure_dirty);
        self.content.take_structure_dirty() || rebuilt
    }

    fn take_focus_reset(&mut self) -> bool {
        std::mem::take(&mut self.focus_first)
    }

    fn name(&self) -> &'static str {
        "ConnectForm"
    }
}

/// The new-connection dialog, open on a blank connection, and the values it
/// reads. A dialog lasts as long as it is open: the next one starts blank.
fn connect_dialog_with_fields(link: &Link) -> (ModalDialog<ConnectForm>, Fields) {
    let fields = Fields::new();
    let save = fields.clone();
    let save_link = link.clone();
    let cancel_link = link.clone();
    let dismiss_link = link.clone();
    let dialog = ModalDialog::new(&t("db-new-connection-title"), ConnectForm::new(&fields))
        .with_test_id("db.connect-dialog")
        .primary_action(&t("db-add-connection"), move || {
            save_link.ask(Intent::SaveConnection(Box::new(save.profile())));
        })
        .secondary_action(&t("db-cancel"), move || {
            cancel_link.ask(Intent::CloseDialog)
        })
        .on_dismiss(move || dismiss_link.ask(Intent::CloseDialog))
        .open(true);
    (dialog, fields)
}

/// The new-connection dialog, open.
pub fn connect_dialog(link: &Link) -> ModalDialog<ConnectForm> {
    connect_dialog_with_fields(link).0
}

/// The statement waiting for confirmation, and where it will run. Open.
pub fn confirm_dialog(link: &Link) -> ModalDialog<Column> {
    let target = link.clone();
    let statement = link.clone();
    let body = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(Box::new(
            Text::dynamic(move || {
                target.read(|model| {
                    if model.active_name.is_empty() {
                        t("db-no-connection-open")
                    } else {
                        format!("This runs on {}.", model.active_name)
                    }
                })
            })
            .with_role(TextRole::Body),
        ))
        .push(Box::new(
            Text::dynamic(move || statement.read(|model| model.pending_sql.clone()))
                .with_role(TextRole::Code),
        ))
        .push(Box::new(
            Text::new(&t("db-cannot-undo"))
                .with_role(TextRole::Caption)
                .dimmed(),
        ));
    let run = link.clone();
    let cancel = link.clone();
    let dismiss = link.clone();
    ModalDialog::new(&t("db-run-statement-title"), body)
        .with_test_id("db.confirm-dialog")
        .with_destructive(true)
        .primary_action(&t("db-run-statement"), move || run.ask(Intent::ConfirmRun))
        .secondary_action(&t("db-cancel"), move || cancel.ask(Intent::CloseDialog))
        .on_dismiss(move || dismiss.ask(Intent::CloseDialog))
        .open(true)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ui::headless::FrameHarness;
    use ui::theme::Theme;

    /// Tab until the field named `label` has the keyboard.
    pub(crate) fn focus_field(harness: &mut FrameHarness, label: &str) {
        for _ in 0..12 {
            let name = harness.focused_access_node().and_then(|node| node.name);
            if name.as_deref() == Some(label) {
                return;
            }
            harness.key("Tab");
        }
        unreachable!("no field named {label} took the keyboard");
    }

    /// Tab moves typing from field to field, and the values the fields hold become the profile.
    #[test]
    fn the_form_takes_typing_in_the_focused_field() {
        let link = Link::new();
        let (dialog, fields) = connect_dialog_with_fields(&link);
        let mut harness =
            FrameHarness::for_root(ui::State::new(), dialog, (900, 800), Theme::dark());
        harness.relayout();

        focus_field(&mut harness, "Connection name");
        harness.type_text("Prod");
        focus_field(&mut harness, "Host");
        harness.type_text("x");
        focus_field(&mut harness, "Database");
        harness.type_text("sales");
        assert_eq!(fields.name.with(Clone::clone), "Prod");
        assert_eq!(fields.host.with(Clone::clone), "localhostx");
        assert_eq!(fields.database.with(Clone::clone), "sales");
        let profile = fields.profile();
        assert_eq!(profile.name, "Prod");
        assert_eq!(profile.engine, Engine::Postgres);
        assert_eq!(profile.port, 5432);
    }
}
