//! App Publisher: the key that signs your apps, and the namespaces the
//! registry lets it publish under.
//!
//! The key is the file `tilo-app` reads, `$XDG_CONFIG_HOME/tilo/publisher-key.json`,
//! which on Tilo OS is private to this app. It never enters the resource: the
//! resource is synced to collaborators and kept in history, and holds only
//! what an application for a namespace needs.

/// The application id.
pub const APP_ID: &str = "so.tilo.publisher";
/// The window and launcher title.
pub const APP_TITLE: &str = "App Publisher";

pub mod locales;

/// Install the catalog once per process, before the first view reads it.
fn install_catalog() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(locales::install);
}

use std::sync::mpsc::Sender;

use registry_client::{RegistryClient, key};
use registry_types::application::{NamespaceClaim, proof_token};
use ui::prelude::*;
use ui::tilo_actions::{
    ActionContract, ActionOutput, ActionSpec, ConfirmationPolicy, DiscoveryScope, ExecutionMode,
    OperationKind, PermissionRequirement, ResultSpec, RiskLevel,
};
use ui::widget::resource_actions::OPERATION_TYPE;
use ui::widget::{Actions as ButtonGroup, BannerKind, ButtonRole, Field, Form, FormSection};

/// What to apply for, and where. Stored, synced and undoable; the key is not
/// part of it.
#[derive(Model, Debug, Clone, PartialEq)]
pub struct Publisher {
    /// The registry the key publishes to.
    pub registry: String,
    /// A domain whose namespace to claim, such as `example.com`.
    pub domain: String,
    /// A forge account whose namespace to claim, such as `github:ada`.
    pub forge_account: String,
    /// How the registry's owner can reach you about the application.
    pub contact: String,
}

impl Default for Publisher {
    fn default() -> Self {
        Self {
            registry: registry_client::DEFAULT_REGISTRY.to_owned(),
            domain: String::new(),
            forge_account: String::new(),
            contact: String::new(),
        }
    }
}

#[resource(kind = "so.tilo.publisher.profile", label = "App Publisher")]
impl Publisher {
    /// Change the registry to publish to.
    #[action]
    pub fn set_registry(
        &mut self,
        #[param("Registry URL")] registry: String,
    ) -> Result<(), &'static str> {
        let loopback = ["http://127.0.0.1", "http://localhost", "http://[::1]"]
            .iter()
            .any(|prefix| registry.starts_with(prefix));
        if !registry.starts_with("https://") && !loopback {
            return Err("a registry URL is https, or http on this machine");
        }
        self.registry = registry;
        Ok(())
    }

    /// Change the domain to claim.
    #[action]
    pub fn set_domain(&mut self, #[param("Domain, such as example.com")] domain: String) {
        self.domain = domain;
    }

    /// Change the forge account to claim.
    #[action]
    pub fn set_forge_account(
        &mut self,
        #[param("Forge account, such as github:ada")] account: String,
    ) {
        self.forge_account = account;
    }

    /// Change the contact address.
    #[action]
    pub fn set_contact(&mut self, #[param("Contact address")] contact: String) {
        self.contact = contact;
    }
}

/// The key on this machine, read from its file.
#[derive(Clone, Debug, PartialEq)]
pub enum KeyState {
    Missing,
    Present {
        public: String,
    },
    /// A file is there but is not a publisher key.
    Unreadable,
}

impl KeyState {
    fn load() -> Self {
        match key::discover() {
            Ok(Some(key)) => Self::Present {
                public: key::public_hex(&key),
            },
            Ok(None) => Self::Missing,
            Err(_) => Self::Unreadable,
        }
    }
}

/// The outcome of the last operation, as the banner that reports it: a
/// severity, a headline and the detail that explains it.
#[derive(Clone, Debug, PartialEq)]
struct Notice {
    kind: BannerKind,
    title: String,
    message: Option<String>,
}

/// A catalog message.
fn say(key: &str) -> String {
    ui::i18n::I18nManager::format(key, &[])
}

impl Notice {
    fn new(kind: BannerKind, key: &str) -> Self {
        Self {
            kind,
            title: say(key),
            message: None,
        }
    }

    /// An outcome worth knowing, with nothing to explain.
    fn info(key: &str) -> Self {
        Self::new(BannerKind::Info, key)
    }

    /// An operation that went through.
    fn success(key: &str) -> Self {
        Self::new(BannerKind::Success, key)
    }

    /// What the person needs to know to go on: a catalog message.
    fn explained(mut self, key: &str) -> Self {
        self.message = Some(say(key));
        self
    }

    /// What the registry or the machine answered, as it said it.
    fn detailed(mut self, detail: impl Into<String>) -> Self {
        self.message = Some(detail.into());
        self
    }

    /// Why an operation failed.
    fn error(detail: impl Into<String>) -> Self {
        Self::new(BannerKind::Error, "publisher-failed-title").detailed(detail)
    }

    fn banner(&self) -> Banner {
        let banner = Banner::new(self.kind, self.title.clone());
        match &self.message {
            Some(message) => banner.with_message(message.clone()),
            None => banner,
        }
    }
}

/// View state: never stored, synced or undone.
#[derive(Clone, Debug, PartialEq)]
struct Pane {
    key: KeyState,
    busy: bool,
    notice: Option<Notice>,
}

impl Pane {
    fn new(key: KeyState) -> Self {
        Self {
            key,
            busy: false,
            notice: None,
        }
    }

    /// Whether there is a key to apply and check with.
    fn has_key(&self) -> bool {
        matches!(self.key, KeyState::Present { .. })
    }
}

/// What each scenario of `--shots --scenario NAME` shows of the key and the
/// last outcome; the claims it stages are in [`App::scenario`].
fn staged(scenario: Option<&str>) -> Pane {
    const PUBLIC: &str = "c3f1a7d2e94b58601fa2d7c9be3048d5a6e1b27f9c4d8350e6a1f72b9d04c8e3";
    let present = KeyState::Present {
        public: PUBLIC.to_owned(),
    };
    let mut pane = Pane::new(present);
    match scenario {
        Some("no-key") => pane.key = KeyState::Missing,
        Some("unreadable") => pane.key = KeyState::Unreadable,
        Some("applied") => {
            pane.notice = Some(
                Notice::info("publisher-status").detailed("pending (lovelace.dev, codeberg:ada)"),
            );
        }
        Some("failed") => {
            pane.notice = Some(Notice::error(
                "could not reach https://apps.tilo.so: connection timed out",
            ));
        }
        Some("copied") => pane.notice = Some(Notice::success("publisher-proof-copied")),
        _ => {}
    }
    pane
}

const OPERATIONS: [(&str, &str); 5] = [
    ("create-key", "Create publisher key"),
    ("copy-proof", "Copy namespace proof"),
    ("copy-secret", "Copy key for CI"),
    ("apply", "Apply for namespaces"),
    ("check-status", "Check application status"),
];

/// Every operation on the key is an action, so the palette, scripts and
/// agents reach the same door as the buttons. Those that create or reveal
/// the key need an owner and a confirmation.
#[expect(
    clippy::expect_used,
    reason = "each operation's payload is a fixed, serializable JSON object"
)]
fn key_resource() -> tilo_actions::Resource {
    let mut resource = tilo_actions::Resource::new(
        "so.tilo.publisher.key:root",
        "so.tilo.publisher.key",
        "Publisher key",
    );
    for (id, label) in OPERATIONS {
        let mut action = ActionSpec::operation(
            id,
            label,
            OperationKind::Effect,
            OPERATION_TYPE,
            &ui::serde_json::json!({ "op": id }),
        )
        .expect("the operation payload is serializable");
        let guarded = matches!(id, "create-key" | "copy-secret");
        action.contract = ActionContract {
            discovery: DiscoveryScope::Launchable,
            execution: ExecutionMode::Background { cancellable: false },
            result: ResultSpec::Job,
            permission: if guarded {
                PermissionRequirement::Admin
            } else {
                PermissionRequirement::Write
            },
            risk: if guarded {
                RiskLevel::Privileged
            } else {
                RiskLevel::Low
            },
            confirmation: if guarded {
                ConfirmationPolicy::Confirm
            } else {
                ConfirmationPolicy::None
            },
            ..ActionContract::default()
        };
        resource.actions.push(action);
    }
    resource
}

fn claims(profile: &Publisher) -> Result<Vec<NamespaceClaim>, String> {
    let mut claims = Vec::new();
    let domain = profile.domain.trim();
    if !domain.is_empty() {
        claims.push(NamespaceClaim::Domain {
            domain: domain.to_owned(),
        });
    }
    let account = profile.forge_account.trim();
    if !account.is_empty() {
        claims.push(registry_client::forge_claim(account).map_err(|error| format!("{error:#}"))?);
    }
    if claims.is_empty() {
        return Err("name a domain or a forge account to claim".to_owned());
    }
    Ok(claims)
}

fn apply(profile: &Publisher) -> Result<String, String> {
    let key = key::resolve(None).map_err(|error| format!("{error:#}"))?;
    let claims = claims(profile)?;
    let client = RegistryClient::new(&profile.registry).map_err(|error| format!("{error:#}"))?;
    let status = registry_client::apply(
        &client,
        &key,
        claims,
        profile.contact.trim().to_owned(),
        String::new(),
    )
    .map_err(|error| format!("{error:#}"))?;
    Ok(registry_client::status_summary(&status))
}

fn check_status(registry: &str) -> Result<Option<String>, String> {
    let key = key::resolve(None).map_err(|error| format!("{error:#}"))?;
    let client = RegistryClient::new(registry).map_err(|error| format!("{error:#}"))?;
    let status = client
        .application(&key::public_hex(&key))
        .map_err(|error| format!("{error:#}"))?;
    Ok(status.as_ref().map(registry_client::status_summary))
}

/// Run a local operation now, on the UI thread: the clipboard lives there.
fn local(op: &str) -> Result<Notice, String> {
    match op {
        "create-key" => {
            let path = key::default_path().ok_or("no configuration directory")?;
            key::create(&path).map_err(|error| format!("{error:#}"))?;
            Ok(Notice::success("publisher-key-created").explained("publisher-key-created-detail"))
        }
        "copy-proof" => {
            let key = key::resolve(None).map_err(|error| format!("{error:#}"))?;
            ui::text_selection::clipboard_copy(&proof_token(&key::public_hex(&key)));
            Ok(Notice::success("publisher-proof-copied"))
        }
        "copy-secret" => {
            let key = key::resolve(None).map_err(|error| format!("{error:#}"))?;
            ui::text_selection::clipboard_copy(&hex::encode(key.to_bytes()));
            Ok(Notice::success("publisher-secret-copied")
                .explained("publisher-secret-copied-detail"))
        }
        other => Err(format!("unknown operation {other:?}")),
    }
}

fn finish(sender: &Sender<Result<ActionOutput, String>>, result: &Result<Notice, String>) {
    let _ = sender.send(
        result
            .as_ref()
            .map(|_| ActionOutput::default())
            .map_err(Clone::clone),
    );
}

#[derive(Clone)]
struct Bindings {
    create: tilo_actions::ActionBinding<()>,
    copy_proof: tilo_actions::ActionBinding<()>,
    copy_secret: tilo_actions::ActionBinding<()>,
    apply: tilo_actions::ActionBinding<()>,
    check_status: tilo_actions::ActionBinding<()>,
}

/// A value to read, copy or paste elsewhere: what it is, the value itself,
/// and, when it needs one, how to use it.
fn value_group(label: &str, value: &str, id: &str, help: Option<&'static str>) -> Box<Column> {
    let mut group = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(0.0)
        .push(Box::new(Text::new(label).with_role(TextRole::Label)))
        .push(Box::new(
            Text::new("publisher-verbatim")
                .with_arg("text", value.to_owned())
                .with_role(TextRole::Code)
                .with_id(id),
        ));
    if let Some(help) = help {
        group = group.push(Box::new(
            Text::new(help).with_role(TextRole::Caption).dimmed(),
        ));
    }
    Box::new(group)
}

/// The key section's body: what the key is and what you can do with it, how
/// to make one, or what is wrong with the file that should be one.
fn key_body(key: &KeyState, bindings: &Bindings) -> Element {
    let mut column = Column::new().with_no_padding().with_intrinsic_height();
    match key {
        KeyState::Missing => {
            column = column
                .push(Box::new(Text::new("publisher-no-key")))
                .push(Box::new(ui::row![
                    Button::new("create-key", "publisher-create-key", None)
                        .on_action(bindings.create.clone())
                ]));
        }
        KeyState::Present { public } => {
            column = column
                .push(value_group(
                    "publisher-public-key-label",
                    public,
                    "public-key",
                    None,
                ))
                .push(value_group(
                    "publisher-proof-label",
                    &proof_token(public),
                    "proof-line",
                    Some("publisher-proof-help"),
                ))
                .push(Box::new(ui::row![
                    Button::new("copy-proof", "publisher-copy-proof", None)
                        .on_action(bindings.copy_proof.clone()),
                    Button::new("copy-secret", "publisher-copy-secret", None)
                        .on_action(bindings.copy_secret.clone()),
                ]));
        }
        KeyState::Unreadable => {
            column = column.push(Box::new(
                Banner::error(say("publisher-key-unreadable-title"))
                    .with_message(say("publisher-key-unreadable")),
            ));
        }
    }
    Box::new(column)
}

/// The namespaces to claim and the registry to claim them from.
fn namespaces_section(cx: &ViewCx<Publisher>) -> FormSection {
    let registry = cx
        .input(|profile| profile.registry.clone(), Publisher::SET_REGISTRY)
        .with_id("registry-input");
    let domain = cx
        .input(|profile| profile.domain.clone(), Publisher::SET_DOMAIN)
        .with_id("domain-input")
        .with_placeholder("publisher-domain-placeholder");
    let forge = cx
        .input(
            |profile| profile.forge_account.clone(),
            Publisher::SET_FORGE_ACCOUNT,
        )
        .with_id("forge-input")
        .with_placeholder("publisher-forge-placeholder");
    let contact = cx
        .input(|profile| profile.contact.clone(), Publisher::SET_CONTACT)
        .with_id("contact-input")
        .with_placeholder("publisher-contact-placeholder");
    FormSection::new("publisher-namespace-heading")
        .field(Field::new("publisher-domain-label", domain).with_help("publisher-domain-help"))
        .field(Field::new("publisher-forge-label", forge).with_help("publisher-forge-help"))
        .field(Field::new("publisher-contact-label", contact).with_help("publisher-contact-help"))
        .field(Field::new("publisher-registry-label", registry))
}

/// What the last operation did, if one is under way or has finished, above the
/// two buttons that start an application. Pinned under the scrolling form, so
/// the outcome and the action it came from are always in view and showing it
/// moves nothing above.
fn footer(pane: &Pane, bindings: &Bindings) -> Element {
    let mut column = Column::new().with_no_padding().with_intrinsic_height();
    if pane.busy {
        column = column.push(Box::new(Spinner::new()));
    }
    if let Some(notice) = &pane.notice {
        column = column.push(Box::new(notice.banner()));
    }
    let usable = pane.has_key();
    let apply = Button::new("apply", "publisher-apply", None)
        .role(ButtonRole::Primary)
        .with_enabled(usable)
        .on_action(bindings.apply.clone());
    let check = Button::new("check-status", "publisher-check-status", None)
        .with_enabled(usable)
        .on_action(bindings.check_status.clone());
    Box::new(column.push(Box::new(ButtonGroup::new(apply).secondary(check))))
}

/// The readable measure the form and its pinned actions share.
fn readable(child: Element) -> Element {
    Box::new(
        Container::new()
            .with_centered_max_width(720.0)
            .with_child(child),
    )
}

/// The app: the key on this machine, and an application for namespaces.
pub struct App;

impl ResourceApp for App {
    type Resource = Publisher;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn scenarios() -> &'static [&'static str] {
        &["no-key", "unreadable", "applied", "failed", "copied"]
    }

    /// A capture never reads this machine's key file or settings: the claims
    /// are filled in here, the key and the last outcome in [`staged`].
    fn scenario(name: Option<&str>) -> Vec<Invoke<Publisher>> {
        if name == Some("no-key") {
            return Vec::new();
        }
        vec![
            Publisher::SET_DOMAIN.with("lovelace.dev".into()),
            Publisher::SET_FORGE_ACCOUNT.with("codeberg:ada".into()),
            Publisher::SET_CONTACT.with("ada@lovelace.dev".into()),
        ]
    }

    #[expect(
        clippy::expect_used,
        reason = "the bindings name the operations key_resource just declared"
    )]
    fn view(cx: &mut ViewCx<Publisher>) -> Element {
        crate::install_catalog();
        let resource = key_resource();
        let bind = |id: &str| {
            tilo_actions::ActionBinding::unit_for(&resource, id)
                .expect("the button matches the exported action")
        };
        let bindings = Bindings {
            create: bind("create-key"),
            copy_proof: bind("copy-proof"),
            copy_secret: bind("copy-secret"),
            apply: bind("apply"),
            check_status: bind("check-status"),
        };

        let task = LatestTask::new(Pane::new(KeyState::load()));
        let pane = task.state();
        let staged_pane = pane.clone();
        cx.on_stage(move |scenario| staged_pane.set(staged(scenario)));
        let key_bindings = bindings.clone();
        let key = ModelView::from_state_by(
            &pane,
            |pane| pane.key.clone(),
            move |pane| key_body(&pane.key, &key_bindings),
        );
        let footer =
            ModelView::from_state_by(&pane, Clone::clone, move |pane| footer(pane, &bindings));

        let form = Form::new()
            .section(FormSection::new("publisher-key-heading").content(Box::new(key)))
            .section(namespaces_section(cx));
        let root = Box::new(
            Column::new()
                .with_no_padding()
                .push(Box::new(
                    Scrollable::new_vertical()
                        .fill_parent_height()
                        .with_child(readable(Box::new(form))),
                ))
                // The pinned outcome and actions sit in a viewport of their own
                // so they keep the form's scrollbar gutter and line up with it;
                // they scroll only in a window too short to show them.
                .push(Box::new(
                    Scrollable::new_vertical()
                        .without_sequential_focus()
                        .fit_content_height()
                        .with_child(readable(Box::new(footer))),
                )),
        );
        let live = cx.live().clone();
        ui::widget::resource_actions::background(root, resource, move |payload| {
            let (sender, receiver) = std::sync::mpsc::channel();
            let op = ui::serde_json::from_slice::<ui::serde_json::Value>(payload)
                .ok()
                .and_then(|value| value.get("op")?.as_str().map(str::to_owned))
                .unwrap_or_default();
            if matches!(op.as_str(), "apply" | "check-status") {
                let profile = live.with(Clone::clone);
                let current = pane.get();
                let completion = task.begin(Pane {
                    busy: true,
                    notice: None,
                    ..current
                });
                std::thread::spawn(move || {
                    let result = if op == "apply" {
                        apply(&profile)
                            .map(|status| Notice::info("publisher-status").detailed(status))
                    } else {
                        check_status(&profile.registry).map(|status| match status {
                            Some(status) => Notice::info("publisher-status").detailed(status),
                            None => Notice::info("publisher-no-application"),
                        })
                    };
                    finish(&sender, &result);
                    let _ = completion.complete(Pane {
                        key: KeyState::load(),
                        busy: false,
                        notice: Some(result.unwrap_or_else(Notice::error)),
                    });
                });
            } else {
                let result = local(&op);
                finish(&sender, &result);
                pane.set(Pane {
                    key: KeyState::load(),
                    busy: false,
                    notice: Some(result.unwrap_or_else(Notice::error)),
                });
            }
            Ok(receiver)
        })
    }
}

ui::dev_reload_entry!(App);
