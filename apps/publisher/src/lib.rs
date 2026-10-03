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

fn install_catalog() {
    locales::install();
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

#[resource(kind = "so.tilo.publisher.profile", label = "Publisher")]
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
    Present { public: String },
    Unreadable(String),
}

impl KeyState {
    fn load() -> Self {
        match key::discover() {
            Ok(Some(key)) => Self::Present {
                public: key::public_hex(&key),
            },
            Ok(None) => Self::Missing,
            Err(error) => Self::Unreadable(format!("{error:#}")),
        }
    }
}

/// The outcome of the last operation, as a catalog message and its argument.
#[derive(Clone, Debug, PartialEq)]
struct Notice {
    message: &'static str,
    detail: String,
    error: bool,
}

impl Notice {
    fn info(message: &'static str, detail: impl Into<String>) -> Self {
        Self {
            message,
            detail: detail.into(),
            error: false,
        }
    }

    fn error(detail: impl Into<String>) -> Self {
        Self {
            message: "publisher-failed",
            detail: detail.into(),
            error: true,
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
            let key = key::create(&path).map_err(|error| format!("{error:#}"))?;
            Ok(Notice::info("publisher-key-created", key::public_hex(&key)))
        }
        "copy-proof" => {
            let key = key::resolve(None).map_err(|error| format!("{error:#}"))?;
            let proof = proof_token(&key::public_hex(&key));
            ui::text_selection::clipboard_copy(&proof);
            Ok(Notice::info("publisher-proof-copied", proof))
        }
        "copy-secret" => {
            let key = key::resolve(None).map_err(|error| format!("{error:#}"))?;
            ui::text_selection::clipboard_copy(&hex::encode(key.to_bytes()));
            Ok(Notice::info("publisher-secret-copied", String::new()))
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
    check_status: tilo_actions::ActionBinding<()>,
}

fn key_section(pane: &Pane, bindings: &Bindings) -> Element {
    let mut column = Column::new().with_no_padding().push(Box::new(
        Text::new("publisher-key-heading").with_role(TextRole::Heading),
    ));
    match &pane.key {
        KeyState::Missing => {
            column = column
                .push(Box::new(Text::new("publisher-no-key")))
                .push(Box::new(
                    Button::new("create-key", "publisher-create-key", None)
                        .on_action(bindings.create.clone()),
                ));
        }
        KeyState::Present { public } => {
            column = column
                .push(Box::new(Text::new("publisher-public-key-label")))
                .push(Box::new(
                    Text::new("publisher-verbatim")
                        .with_arg("text", public.clone())
                        .with_role(TextRole::Code)
                        .with_id("public-key"),
                ))
                .push(Box::new(Text::new("publisher-proof-label")))
                .push(Box::new(
                    Text::new("publisher-verbatim")
                        .with_arg("text", proof_token(public))
                        .with_role(TextRole::Code)
                        .with_id("proof-line"),
                ))
                .push(Box::new(ui::row![
                    Button::new("copy-proof", "publisher-copy-proof", None)
                        .on_action(bindings.copy_proof.clone()),
                    Button::new("copy-secret", "publisher-copy-secret", None)
                        .on_action(bindings.copy_secret.clone()),
                    Button::new("check-status", "publisher-check-status", None)
                        .on_action(bindings.check_status.clone()),
                ]));
        }
        KeyState::Unreadable(reason) => {
            column = column.push(Box::new(Banner::error(reason.clone())));
        }
    }
    if pane.busy {
        column = column.push(Box::new(Spinner::new()));
    }
    if let Some(notice) = &pane.notice {
        if notice.error {
            column = column.push(Box::new(Banner::error(ui::i18n::I18nManager::format(
                "publisher-failed-title",
                &[],
            ))));
        }
        column = column.push(Box::new(
            Text::new(notice.message)
                .with_arg("detail", notice.detail.clone())
                .with_id("notice"),
        ));
    }
    Box::new(column)
}

/// The app: the key on this machine, and an application for namespaces.
pub struct App;

impl ResourceApp for App {
    type Resource = Publisher;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;

    #[expect(
        clippy::expect_used,
        reason = "the bindings name the operations key_resource just declared"
    )]
    fn view(cx: &mut ViewCx<Publisher>) -> Element {
        crate::install_catalog();
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

        let resource = key_resource();
        let bind = |id: &str| {
            tilo_actions::ActionBinding::unit_for(&resource, id)
                .expect("the button matches the exported action")
        };
        let bindings = Bindings {
            create: bind("create-key"),
            copy_proof: bind("copy-proof"),
            copy_secret: bind("copy-secret"),
            check_status: bind("check-status"),
        };
        let apply_button = Button::new("apply", "publisher-apply", None).on_action(bind("apply"));

        let task = LatestTask::new(Pane {
            key: KeyState::load(),
            busy: false,
            notice: None,
        });
        let pane = task.state();
        let key = ModelView::from_state_by(&pane, Clone::clone, move |pane| {
            key_section(pane, &bindings)
        });

        let root = Box::new(ui::column![
            key,
            Text::new("publisher-namespace-heading").with_role(TextRole::Heading),
            Text::new("publisher-namespace-help"),
            labeled_field("publisher-domain-label", domain),
            labeled_field("publisher-forge-label", forge),
            labeled_field("publisher-contact-label", contact),
            labeled_field("publisher-registry-label", registry),
            apply_button,
        ]);
        let live = cx.live().clone();
        ui::widget::resource_actions::background(root, resource, move |payload| {
            let (sender, receiver) = std::sync::mpsc::channel();
            let op = ui::serde_json::from_slice::<ui::serde_json::Value>(payload)
                .ok()
                .and_then(|value| value.get("op")?.as_str().map(str::to_owned))
                .unwrap_or_default();
            if op == "apply" || op == "check-status" {
                let profile = live.with(Clone::clone);
                let current = pane.get();
                let completion = task.begin(Pane {
                    busy: true,
                    notice: None,
                    ..current
                });
                std::thread::spawn(move || {
                    let result = if op == "apply" {
                        apply(&profile).map(|status| Notice::info("publisher-status", status))
                    } else {
                        check_status(&profile.registry).map(|status| match status {
                            Some(status) => Notice::info("publisher-status", status),
                            None => Notice::info("publisher-no-application", String::new()),
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
