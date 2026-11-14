//! Outline, Linear and mail accounts on the import page: the person types a source's address
//! and key once, reviews what it holds, chooses what comes over and starts it; the migration's
//! journal resumes a stopped run and never lands an item twice
//! ([`imager_backend::service_sources`]).
//!
//! Agents and scripts reach the same steps through the import page's actions, except typing the
//! key or password: `connect-service` opens the form for the person, so a key never travels in
//! an action's arguments or its History.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

use imager_backend::migration_store::MigrationStore;
use imager_backend::service_sources::{
    self, InboxLanding, JournalState, ServiceAddress, ServiceError, ServiceJournal, ServiceKind,
    ServiceReport, ServiceReview,
};
use ui::icons::Icon;
use ui::widget::{
    Button, ButtonRole, Column, Field, LabelPlacement, ListRow, Row, Section, SectionHeader,
    Spacer, Text, TextInput, TextInputKind,
};
use zeroize::Zeroizing;

use super::strings::{t, t_args};

#[derive(Clone, Copy, Debug)]
enum Command {
    Open(ServiceKind),
    Connect,
    Close,
    Review(usize),
    Choose(usize, usize),
    Start(usize),
    Cancel,
}

enum Done {
    Connected(Result<String, ServiceError>),
    Reviewed(Result<ServiceReview, ServiceError>),
    Progress(ServiceJournal),
    Ran(ServiceJournal, Result<ServiceReport, ServiceError>),
}

pub(super) struct Services {
    config_root: Option<PathBuf>,
    home: Option<PathBuf>,
    store: Option<Arc<MigrationStore>>,
    connected: Vec<(String, ServiceKind, String)>,
    journals: Vec<ServiceJournal>,
    form: Option<ServiceKind>,
    /// The address, the login and the key or password, as the person types them.
    fields: [ui::LocalState<String>; 3],
    work: Option<mpsc::Receiver<Done>>,
    cancel: Arc<AtomicBool>,
    commands: Rc<RefCell<Vec<Command>>>,
    status: Option<(String, bool)>,
}

impl Services {
    /// The sources of the home this window runs in, their journals kept in `store`.
    pub(super) fn new(store: Option<Arc<MigrationStore>>) -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute());
        Self::at(store, service_sources::config_root(), home)
    }

    pub(super) fn at(
        store: Option<Arc<MigrationStore>>,
        config_root: Option<PathBuf>,
        home: Option<PathBuf>,
    ) -> Self {
        let journals = store
            .as_ref()
            .and_then(|store| store.load_services().ok())
            .unwrap_or_default();
        let connected = config_root
            .as_deref()
            .map(service_sources::connected)
            .unwrap_or_default()
            .into_iter()
            .map(|(id, kind, title)| (id.0, kind, title))
            .collect();
        Self {
            config_root,
            home,
            store,
            connected,
            journals,
            form: None,
            fields: std::array::from_fn(|_| ui::LocalState::new(String::new())),
            work: None,
            cancel: Arc::new(AtomicBool::new(false)),
            commands: Rc::new(RefCell::new(Vec::new())),
            status: None,
        }
    }

    pub(super) fn busy(&self) -> bool {
        self.work.is_some()
    }

    fn journal_index(&self, source: &str) -> Option<usize> {
        self.journals
            .iter()
            .position(|journal| journal.id == source)
    }

    /// What agents read: the connected sources and each review, never a key.
    pub(super) fn value(&self) -> serde_json::Value {
        serde_json::json!({
            "connected": self.connected.iter().map(|(account, kind, title)| serde_json::json!({
                "account": account, "service": kind.key(), "title": title,
            })).collect::<Vec<_>>(),
            "reviews": self.journals.iter().map(|journal| serde_json::json!({
                "source": journal.id,
                "service": journal.review.kind.key(),
                "title": journal.review.title,
                "state": journal.state,
                "landed": journal.landed.len(),
                "chosen": journal.review.chosen_count(),
                "report": journal.report,
                "failure": journal.failure,
                "groups": journal.review.groups.iter().map(|group| serde_json::json!({
                    "group": group.key, "title": group.title,
                    "items": group.items.len(), "chosen": group.chosen,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "form": self.form.map(ServiceKind::key),
            "working": self.busy(),
            "status": self.status.as_ref().map(|(status, _)| status),
        })
    }

    /// An agent's or script's step. Answers what the page now shows.
    pub(super) fn invoke(&mut self, step: Step) -> Result<serde_json::Value, String> {
        let command = match step {
            Step::Connect(service) => Command::Open(
                ServiceKind::from_key(&service).ok_or_else(|| t("imager-services-unknown"))?,
            ),
            Step::Review(account) => Command::Review(
                self.connected
                    .iter()
                    .position(|(id, _, _)| *id == account)
                    .ok_or_else(|| t("imager-services-unknown-source"))?,
            ),
            Step::Choose(source, group, chosen) => {
                let index = self
                    .journal_index(&source)
                    .ok_or_else(|| t("imager-services-unknown-source"))?;
                let group = self.journals[index]
                    .review
                    .groups
                    .iter()
                    .position(|candidate| candidate.key == group)
                    .ok_or_else(|| t("imager-services-unknown-source"))?;
                if self.journals[index].review.groups[group].chosen == chosen {
                    return Ok(self.value());
                }
                Command::Choose(index, group)
            }
            Step::Start(source) => Command::Start(
                self.journal_index(&source)
                    .ok_or_else(|| t("imager-services-unknown-source"))?,
            ),
            Step::Cancel => {
                if !self.busy() {
                    return Err(t("imager-services-nothing-running"));
                }
                Command::Cancel
            }
        };
        self.apply(command)?;
        Ok(self.value())
    }

    /// Run the steps the page's buttons asked for. Whether anything changed.
    pub(super) fn drain(&mut self) -> bool {
        let commands = std::mem::take(&mut *self.commands.borrow_mut());
        let changed = !commands.is_empty();
        for command in commands {
            if let Err(error) = self.apply(command) {
                self.status = Some((error, true));
            }
        }
        changed
    }

    fn apply(&mut self, command: Command) -> Result<(), String> {
        if self.busy() && !matches!(command, Command::Cancel | Command::Choose(..)) {
            return Err(t("imager-services-busy"));
        }
        let (Some(config_root), Some(store)) = (self.config_root.clone(), self.store.clone())
        else {
            return Err(t("imager-services-unavailable"));
        };
        match command {
            Command::Open(kind) => {
                for field in &self.fields {
                    field.set(String::new());
                }
                if kind == ServiceKind::Linear {
                    self.fields[0].set(service_sources::LINEAR_API.to_owned());
                }
                self.form = Some(kind);
                self.status = None;
            }
            Command::Close => {
                self.form = None;
                self.fields[2].set(String::new());
            }
            Command::Connect => {
                let kind = self.form.ok_or_else(|| t("imager-services-unknown"))?;
                let address = ServiceAddress {
                    kind,
                    server: self.fields[0].get(),
                    login: self.fields[1].get(),
                    secret: Zeroizing::new(self.fields[2].get()),
                };
                self.fields[2].set(String::new());
                let (send, receive) = mpsc::channel();
                std::thread::spawn(move || {
                    let connected = service_sources::connect(&config_root, address);
                    let _ = send.send(Done::Connected(connected.map(|id| id.0)));
                });
                self.work = Some(receive);
                self.status = Some((t("imager-services-connecting"), false));
            }
            Command::Review(index) => {
                let (account, _, _) = self
                    .connected
                    .get(index)
                    .cloned()
                    .ok_or_else(|| t("imager-services-unknown-source"))?;
                let (send, receive) = mpsc::channel();
                std::thread::spawn(move || {
                    let _ = send.send(Done::Reviewed(service_sources::review(
                        &config_root,
                        &account,
                    )));
                });
                self.work = Some(receive);
                self.status = Some((t("imager-services-reviewing"), false));
            }
            Command::Choose(index, group) => {
                let journal = self
                    .journals
                    .get_mut(index)
                    .ok_or_else(|| t("imager-services-unknown-source"))?;
                if journal.state == JournalState::Running {
                    return Err(t("imager-services-busy"));
                }
                let entry = journal
                    .review
                    .groups
                    .get_mut(group)
                    .ok_or_else(|| t("imager-services-unknown-source"))?;
                entry.chosen = !entry.chosen;
                store.save_service(journal)?;
            }
            Command::Start(index) => {
                let home = self
                    .home
                    .clone()
                    .ok_or_else(|| t("imager-services-unavailable"))?;
                let mut journal = self
                    .journals
                    .get(index)
                    .cloned()
                    .ok_or_else(|| t("imager-services-unknown-source"))?;
                if journal.review.chosen_count() == 0 {
                    return Err(t("imager-services-nothing-chosen"));
                }
                self.cancel.store(false, Ordering::Relaxed);
                let cancel = Arc::clone(&self.cancel);
                let (send, receive) = mpsc::channel();
                std::thread::spawn(move || {
                    let mut landing = InboxLanding::new(&home);
                    let progress = send.clone();
                    let result = service_sources::run(
                        &config_root,
                        &mut journal,
                        &mut landing,
                        &mut |journal| {
                            let _ = progress.send(Done::Progress(journal.clone()));
                            store.save_service(journal)
                        },
                        &cancel,
                    );
                    let _ = send.send(Done::Ran(journal, result));
                });
                self.work = Some(receive);
                self.status = Some((t("imager-services-bringing"), false));
            }
            Command::Cancel => self.cancel.store(true, Ordering::Relaxed),
        }
        Ok(())
    }

    /// Take what the source's worker finished. Whether anything changed.
    pub(super) fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Some(receive) = &self.work {
            let done = match receive.try_recv() {
                Ok(done) => done,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.work = None;
                    self.status = Some((t("imager-services-stopped"), true));
                    return true;
                }
            };
            changed = true;
            match done {
                Done::Connected(result) => {
                    self.work = None;
                    match result {
                        Ok(account) => {
                            self.form = None;
                            self.refresh_connected();
                            self.status = Some((t("imager-services-connected"), false));
                            if let Some(index) =
                                self.connected.iter().position(|(id, _, _)| *id == account)
                            {
                                let _ = self.apply(Command::Review(index));
                            }
                        }
                        Err(error) => self.status = Some((refusal(&error), true)),
                    }
                }
                Done::Reviewed(result) => {
                    self.work = None;
                    match result {
                        Ok(review) => self.reviewed(review),
                        Err(error) => self.status = Some((refusal(&error), true)),
                    }
                }
                Done::Progress(journal) => self.keep(journal),
                Done::Ran(journal, result) => {
                    self.work = None;
                    self.keep(journal);
                    self.status = Some(match result {
                        Ok(report) => (report_text(&report), false),
                        Err(ServiceError::Cancelled) => (t("imager-services-stopped"), false),
                        Err(error) => (refusal(&error), true),
                    });
                }
            }
        }
        changed
    }

    fn refresh_connected(&mut self) {
        self.connected = self
            .config_root
            .as_deref()
            .map(service_sources::connected)
            .unwrap_or_default()
            .into_iter()
            .map(|(id, kind, title)| (id.0, kind, title))
            .collect();
    }

    fn reviewed(&mut self, review: ServiceReview) {
        let mut journal = ServiceJournal::new(review.clone());
        if let Some(index) = self.journal_index(&journal.id) {
            journal = self.journals[index].clone();
            journal.reviewed_again(review);
        }
        if let Some(store) = &self.store
            && let Err(error) = store.save_service(&journal)
        {
            self.status = Some((error, true));
            return;
        }
        self.status = Some((
            t_args(
                "imager-services-reviewed",
                &[("count", (journal.review.item_count() as i64).into())],
            ),
            false,
        ));
        self.keep(journal);
    }

    fn keep(&mut self, journal: ServiceJournal) {
        match self.journal_index(&journal.id) {
            Some(index) => self.journals[index] = journal,
            None => self.journals.push(journal),
        }
    }

    fn button(&self, id: &str, label: &str, command: Command) -> Button {
        let sink = Rc::clone(&self.commands);
        Button::new(id, label)
            .control_size(ui::widget::ControlSize::Regular)
            .on_effect(move || sink.borrow_mut().push(command))
    }

    /// The page's section: connect a source, review it, choose and bring it over.
    pub(super) fn section(&self) -> Section {
        let mut body: Vec<Box<dyn ui::widget::Widget>> =
            vec![Box::new(Text::new(&t("imager-services-body")).dimmed())];
        if let Some(kind) = self.form {
            body.push(Box::new(self.form_view(kind)));
        } else {
            body.push(Box::new(
                Row::new()
                    .push(Box::new(self.button(
                        "import.services.outline",
                        &t("imager-services-outline"),
                        Command::Open(ServiceKind::Outline),
                    )))
                    .push(Box::new(self.button(
                        "import.services.linear",
                        &t("imager-services-linear"),
                        Command::Open(ServiceKind::Linear),
                    )))
                    .push(Box::new(self.button(
                        "import.services.mail",
                        &t("imager-services-mail"),
                        Command::Open(ServiceKind::Mail),
                    ))),
            ));
        }
        for (index, (account, kind, title)) in self.connected.iter().enumerate() {
            if self
                .journals
                .iter()
                .any(|journal| journal.review.account == *account)
            {
                continue;
            }
            body.push(Box::new(
                ListRow::new(format!("import.services.source.{index}"), title.clone())
                    .icon(icon(*kind))
                    .trailing(Box::new(
                        self.button(
                            &format!("import.services.review.{index}"),
                            &t("imager-services-review"),
                            Command::Review(index),
                        )
                        .with_enabled(!self.busy()),
                    )),
            ));
        }
        for (index, journal) in self.journals.iter().enumerate() {
            body.push(Box::new(self.review_view(index, journal)));
        }
        if let Some((status, error)) = &self.status {
            let text = Text::new(status);
            body.push(Box::new(if *error {
                text.with_live(ui::a11y::Politeness::Assertive)
            } else {
                text.dimmed()
            }));
        }
        Section::new(body).with_header(SectionHeader::heading(t("imager-services-heading")))
    }

    fn form_view(&self, kind: ServiceKind) -> Column {
        let field = |index: usize, id: &str, label: &str, input: TextInputKind, secret: bool| {
            Field::new(
                t(label),
                TextInput::from_state(&self.fields[index])
                    .with_id(&format!("import.services.{id}"))
                    .with_kind(input)
                    .with_secret(secret),
            )
            .with_label_placement(Some(LabelPlacement::Above))
        };
        let mut form = Column::new().with_rhythm(ui::tokens::Rhythm::Related);
        match kind {
            ServiceKind::Outline => {
                form = form
                    .push(Box::new(field(
                        0,
                        "server",
                        "imager-services-outline-address",
                        TextInputKind::Url,
                        false,
                    )))
                    .push(Box::new(field(
                        2,
                        "secret",
                        "imager-services-api-key",
                        TextInputKind::Secret,
                        true,
                    )));
            }
            ServiceKind::Linear => {
                form = form.push(Box::new(field(
                    2,
                    "secret",
                    "imager-services-api-key",
                    TextInputKind::Secret,
                    true,
                )));
            }
            ServiceKind::Mail => {
                form = form
                    .push(Box::new(field(
                        1,
                        "login",
                        "imager-services-mail-address",
                        TextInputKind::Username,
                        false,
                    )))
                    .push(Box::new(field(
                        2,
                        "secret",
                        "imager-services-mail-password",
                        TextInputKind::Password,
                        true,
                    )))
                    .push(Box::new(field(
                        0,
                        "server",
                        "imager-services-mail-server",
                        TextInputKind::Url,
                        false,
                    )));
            }
        }
        form.push(Box::new(Text::new(&t("imager-services-key-note")).dimmed()))
            .push(Box::new(
                Row::new()
                    .push(Box::new(
                        self.button(
                            "import.services.cancel",
                            &t("imager-services-cancel"),
                            Command::Close,
                        )
                        .role(ButtonRole::Plain),
                    ))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(
                        self.button(
                            "import.services.connect",
                            &t("imager-services-connect"),
                            Command::Connect,
                        )
                        .role(ButtonRole::Primary)
                        .with_enabled(!self.busy()),
                    )),
            ))
    }

    fn review_view(&self, index: usize, journal: &ServiceJournal) -> Column {
        let review = &journal.review;
        let mut card = Column::new()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(Box::new(
                ListRow::new(
                    format!("import.services.review-title.{index}"),
                    review.title.clone(),
                )
                .icon(icon(review.kind))
                .secondary(t_args(
                    "imager-services-chosen",
                    &[
                        ("chosen", (review.chosen_count() as i64).into()),
                        ("count", (review.item_count() as i64).into()),
                        ("landed", (journal.landed.len() as i64).into()),
                    ],
                )),
            ));
        let running = journal.state == JournalState::Running;
        for (group_index, group) in review.groups.iter().enumerate() {
            let shown: Vec<String> = group
                .items
                .iter()
                .take(4)
                .map(|item| item.title.clone())
                .collect();
            let mut detail = shown.join(", ");
            if group.items.len() > shown.len() {
                detail.push_str(&t_args(
                    "imager-services-more",
                    &[("count", ((group.items.len() - shown.len()) as i64).into())],
                ));
            }
            card = card.push(Box::new(
                ListRow::new(
                    format!("import.services.group.{index}.{group_index}"),
                    t_args(
                        "imager-services-group",
                        &[
                            ("title", group.title.clone().into()),
                            ("count", (group.items.len() as i64).into()),
                        ],
                    ),
                )
                .icon(if group.chosen {
                    Icon::Check
                } else {
                    Icon::Minus
                })
                .secondary(detail)
                .trailing(Box::new(
                    self.button(
                        &format!("import.services.choose.{index}.{group_index}"),
                        &t(if group.chosen {
                            "imager-services-leave-out"
                        } else {
                            "imager-services-bring"
                        }),
                        Command::Choose(index, group_index),
                    )
                    .role(ButtonRole::Plain)
                    .with_enabled(!running),
                )),
            ));
        }
        let mut actions = Row::new();
        if let Some(report) = &journal.report {
            actions = actions.push(Box::new(Text::new(&report_text(report)).dimmed()));
        } else if let Some(failure) = &journal.failure {
            actions = actions.push(Box::new(Text::new(failure).dimmed()));
        }
        actions = actions.push(Box::new(Spacer::new()));
        let reconnected = self
            .connected
            .iter()
            .position(|(account, _, _)| *account == review.account);
        actions = if journal.state == JournalState::Complete {
            // Everything reviewed is here: a new review finds what the source gained since.
            match reconnected {
                Some(source) => actions.push(Box::new(
                    self.button(
                        &format!("import.services.review-again.{index}"),
                        &t("imager-services-review-again"),
                        Command::Review(source),
                    )
                    .with_enabled(!self.busy()),
                )),
                None => actions,
            }
        } else if running && self.busy() {
            actions.push(Box::new(self.button(
                &format!("import.services.stop.{index}"),
                &t("imager-services-stop"),
                Command::Cancel,
            )))
        } else {
            actions.push(Box::new(
                self.button(
                    &format!("import.services.start.{index}"),
                    &t(if journal.landed.is_empty() {
                        "imager-services-start"
                    } else {
                        "imager-services-continue"
                    }),
                    Command::Start(index),
                )
                .role(ButtonRole::Primary)
                .with_enabled(!self.busy() && review.chosen_count() > 0),
            ))
        };
        card.push(Box::new(actions))
    }
}

/// An agent's or script's step on a service source.
pub(super) enum Step {
    Connect(String),
    Review(String),
    Choose(String, String, bool),
    Start(String),
    Cancel,
}

fn icon(kind: ServiceKind) -> Icon {
    match kind {
        ServiceKind::Outline => Icon::FileText,
        ServiceKind::Linear => Icon::ListChecks,
        ServiceKind::Mail => Icon::Mail,
    }
}

fn refusal(error: &ServiceError) -> String {
    match error {
        ServiceError::InvalidAddress => t("imager-services-invalid"),
        ServiceError::Refused => t("imager-services-refused"),
        ServiceError::AlreadyThere => t("imager-services-already-there"),
        ServiceError::TooLarge => t("imager-services-too-large"),
        other => t_args(
            "imager-services-failed",
            &[("reason", other.to_string().into())],
        ),
    }
}

fn report_text(report: &ServiceReport) -> String {
    t_args(
        "imager-services-report",
        &[
            ("landed", (report.landed as i64).into()),
            ("already", (report.already_there as i64).into()),
            ("new", (report.not_reviewed as i64).into()),
        ],
    )
}

impl Services {
    /// The page as a person sees it after reviewing an Outline team and bringing a Linear
    /// workspace over: what a screenshot of the review shows, without reaching either service.
    pub(super) fn staged_for_shot(store: Arc<MigrationStore>) -> Self {
        use imager_backend::service_sources::{ReviewGroup, ReviewItem};
        let item = |key: &str, title: &str, facts: &[&str]| ReviewItem {
            key: key.into(),
            title: title.into(),
            facts: facts.iter().map(|fact| (*fact).to_owned()).collect(),
        };
        let mut services = Self::at(Some(Arc::clone(&store)), None, None);
        let outline = ServiceReview {
            kind: ServiceKind::Outline,
            account: "outline-shot".into(),
            title: "Outline: Acme".into(),
            groups: vec![
                ReviewGroup {
                    key: "handbook".into(),
                    title: "Handbook".into(),
                    items: vec![
                        item("c-handbook", "Handbook", &[]),
                        item("d-welcome", "Welcome", &["Handbook"]),
                        item(
                            "d-onboarding",
                            "Onboarding checklist",
                            &["Handbook", "Welcome"],
                        ),
                        item("d-holidays", "Holidays", &["Handbook"]),
                        item("d-expenses", "Expenses", &["Handbook"]),
                        item("d-security", "Security basics", &["Handbook"]),
                    ],
                    chosen: true,
                },
                ReviewGroup {
                    key: "product".into(),
                    title: "Product".into(),
                    items: vec![
                        item("c-product", "Product", &[]),
                        item("d-roadmap", "Roadmap 2027", &["Product"]),
                        item("d-pricing", "Pricing", &["Product"]),
                    ],
                    chosen: true,
                },
                ReviewGroup {
                    key: "archive".into(),
                    title: "Archive".into(),
                    items: vec![
                        item("c-archive", "Archive", &[]),
                        item("d-old", "Old plans", &["Archive"]),
                    ],
                    chosen: false,
                },
            ],
        };
        let mut linear = ServiceJournal::new(ServiceReview {
            kind: ServiceKind::Linear,
            account: "linear-shot".into(),
            title: "Linear: Acme".into(),
            groups: vec![ReviewGroup {
                key: "eng".into(),
                title: "Engineering".into(),
                items: vec![
                    item("i-1", "Ship the migration", &["ENG-1", "In Progress"]),
                    item("i-2", "Write the review page", &["ENG-2", "Todo"]),
                    item("i-3", "Old importer gone", &["ENG-3", "Done"]),
                ],
                chosen: true,
            }],
        });
        linear.landed = ["i-1", "i-2", "i-3"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        linear.state = JournalState::Complete;
        linear.report = Some(ServiceReport {
            landed: 3,
            ..ServiceReport::default()
        });
        services.journals = vec![ServiceJournal::new(outline), linear];
        services.connected = vec![(
            "ops@acme.test".into(),
            ServiceKind::Mail,
            "ops@acme.test".into(),
        )];
        services.status = Some((
            t_args("imager-services-reviewed", &[("count", 11_i64.into())]),
            false,
        ));
        services
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use test_http::{Reply, Responder};

    const KEY: &str = "ol_api_page_key";

    fn outline() -> Responder {
        Responder::handle(|request| {
            if request.header("authorization") != Some(format!("Bearer {KEY}").as_str()) {
                return Some(Reply::status(401));
            }
            let asked: Value = serde_json::from_str(&request.body_text()).unwrap_or(Value::Null);
            let first = asked["offset"].as_u64().unwrap_or(0) == 0;
            let data = match request.target.as_str() {
                "/api/auth.info" => json!({"user": {"name": "Ada"}, "team": {"name": "Acme"}}),
                "/api/collections.list" if first => json!([
                    {"id": "col-a", "name": "Handbook", "description": "", "url": "/collection/a"},
                    {"id": "col-b", "name": "Archive", "description": "", "url": "/collection/b"}
                ]),
                "/api/documents.list" if first => {
                    let collection = asked["collectionId"].as_str().unwrap_or_default();
                    json!([{
                        "id": format!("doc-{collection}"), "urlId": format!("u{collection}"),
                        "title": format!("Page of {collection}"), "text": "Words.",
                        "collectionId": collection, "parentDocumentId": null,
                        "url": format!("/doc/page-u{collection}")
                    }])
                }
                _ => json!([]),
            };
            Some(
                Reply::status(200)
                    .header("content-type", "application/json")
                    .body(json!({ "data": data }).to_string()),
            )
        })
    }

    fn settle(services: &mut Services) {
        let until = std::time::Instant::now() + tilo_test_env::hang_bound();
        while services.busy() {
            services.poll();
            assert!(
                std::time::Instant::now() < until,
                "the source's worker never finished"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn the_page_connects_reviews_chooses_and_brings_a_source_over() {
        let server = outline();
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("home");
        std::fs::create_dir(&home).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let store =
            Arc::new(MigrationStore::open_at(&temporary.path().join("migrations")).unwrap());
        let mut services = Services::at(
            Some(Arc::clone(&store)),
            Some(home.join(".config/tilo")),
            Some(home.clone()),
        );

        // An agent opens the form; only the person types the key.
        let opened = services.invoke(Step::Connect("outline".into())).unwrap();
        assert_eq!(opened["form"], "outline");
        services.fields[0].set(server.url());
        services.fields[2].set(KEY.into());
        services.commands.borrow_mut().push(Command::Connect);
        services.drain();
        assert_eq!(
            services.fields[2].get(),
            "",
            "the typed key leaves the field at once"
        );
        settle(&mut services);
        // Connecting reviews the source straight away.
        settle(&mut services);
        let shown = services.value();
        assert_eq!(shown["connected"][0]["title"], "Outline: Acme");
        let source = shown["reviews"][0]["source"].as_str().unwrap().to_owned();
        assert_eq!(shown["reviews"][0]["chosen"], 4);

        services
            .invoke(Step::Choose(source.clone(), "col-b".into(), false))
            .unwrap();
        services.invoke(Step::Start(source)).unwrap();
        settle(&mut services);
        let shown = services.value();
        assert_eq!(shown["reviews"][0]["state"], "complete");
        assert_eq!(shown["reviews"][0]["landed"], 2);
        assert!(!shown.to_string().contains(KEY), "{shown}");

        let inbox = InboxLanding::new(&home).inbox("so.tilo.notes", "pages");
        assert_eq!(
            std::fs::read_dir(&inbox).unwrap().count(),
            2,
            "the archive stayed out"
        );

        // The journal outlives the window: a page opened later shows the same review.
        drop(services);
        let reopened = Services::at(
            Some(Arc::clone(&store)),
            Some(home.join(".config/tilo")),
            Some(home),
        );
        assert_eq!(reopened.value()["reviews"][0]["landed"], 2);
    }
}
