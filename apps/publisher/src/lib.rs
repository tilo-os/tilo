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

use registry_client::{RegistryClient, key};
use registry_types::application::{NamespaceClaim, proof_token};
use ui::prelude::*;

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

/// What the publisher does with the key on this machine and the registry,
/// for every caller: the buttons, the palette, scripts and agents. Creating
/// or revealing the key needs an owner and a confirmation.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.publisher.key")]
pub enum KeyEffect {
    /// Create publisher key.
    #[effect(privileged, permission = "admin")]
    CreateKey,
    /// Copy namespace proof.
    CopyProof,
    /// Copy key for CI.
    #[effect(privileged, permission = "admin")]
    CopySecret,
    /// Apply for namespaces.
    #[effect(job)]
    ApplyForNamespaces,
    /// Check application status.
    #[effect(job)]
    CheckStatus,
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

/// Create or copy the key now, on the UI thread: the clipboard lives there.
fn local(effect: &KeyEffect) -> Result<Notice, String> {
    match effect {
        KeyEffect::CreateKey => {
            let path = key::default_path().ok_or("no configuration directory")?;
            let key = key::create(&path).map_err(|error| format!("{error:#}"))?;
            Ok(Notice::info("publisher-key-created", key::public_hex(&key)))
        }
        KeyEffect::CopyProof => {
            let key = key::resolve(None).map_err(|error| format!("{error:#}"))?;
            let proof = proof_token(&key::public_hex(&key));
            ui::text_selection::clipboard_copy(&proof);
            Ok(Notice::info("publisher-proof-copied", proof))
        }
        KeyEffect::CopySecret => {
            let key = key::resolve(None).map_err(|error| format!("{error:#}"))?;
            ui::text_selection::clipboard_copy(&hex::encode(key.to_bytes()));
            Ok(Notice::info("publisher-secret-copied", String::new()))
        }
        KeyEffect::ApplyForNamespaces | KeyEffect::CheckStatus => {
            Err(format!("`{}` runs as a job", effect.id()))
        }
    }
}

/// Ask the registry, off the UI thread.
fn remote(effect: &KeyEffect, profile: &Publisher) -> Result<Notice, String> {
    if *effect == KeyEffect::ApplyForNamespaces {
        return apply(profile).map(|status| Notice::info("publisher-status", status));
    }
    check_status(&profile.registry).map(|status| match status {
        Some(status) => Notice::info("publisher-status", status),
        None => Notice::info("publisher-no-application", String::new()),
    })
}

/// The effects the key on this machine and the work in flight rule out now.
fn unavailable(pane: &Pane) -> Vec<EffectStatus> {
    let mut status = Vec::new();
    if matches!(pane.key, KeyState::Present { .. }) {
        status.push(EffectStatus::unavailable(
            KeyEffect::CREATE_KEY,
            "key-exists",
            "This machine already has a publisher key",
        ));
    } else {
        for effect in [
            KeyEffect::COPY_PROOF,
            KeyEffect::COPY_SECRET,
            KeyEffect::APPLY_FOR_NAMESPACES,
            KeyEffect::CHECK_STATUS,
        ] {
            status.push(EffectStatus::unavailable(
                effect,
                "no-key",
                "Create a publisher key first",
            ));
        }
        if !matches!(pane.key, KeyState::Missing) {
            status.push(EffectStatus::unavailable(
                KeyEffect::CREATE_KEY,
                "key-unreadable",
                "The key file on this machine cannot be read",
            ));
        }
    }
    if pane.busy && matches!(pane.key, KeyState::Present { .. }) {
        for effect in [KeyEffect::APPLY_FOR_NAMESPACES, KeyEffect::CHECK_STATUS] {
            status.push(EffectStatus::unavailable(
                effect,
                "busy",
                "The registry has not answered yet",
            ));
        }
    }
    status
}

fn key_section(pane: &Pane, actions: &Actions<Publisher>) -> Element {
    let button = |id: &str, label: &str, effect: &KeyEffect| {
        actions.bind_effect_invocation(Button::new(id, label, None), effect)
    };
    let mut column = Column::new().with_no_padding().push(Box::new(
        Text::new("publisher-key-heading").with_role(TextRole::Heading),
    ));
    match &pane.key {
        KeyState::Missing => {
            column = column
                .push(Box::new(Text::new("publisher-no-key")))
                .push(Box::new(button(
                    "create-key",
                    "publisher-create-key",
                    &KeyEffect::CreateKey,
                )));
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
                    button("copy-proof", "publisher-copy-proof", &KeyEffect::CopyProof),
                    button(
                        "copy-secret",
                        "publisher-copy-secret",
                        &KeyEffect::CopySecret
                    ),
                    button(
                        "check-status",
                        "publisher-check-status",
                        &KeyEffect::CheckStatus
                    ),
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

/// The public half of the key a capture shows: a capture never reads the
/// key on the machine it runs on.
const CAPTURE_PUBLIC_KEY: &str = "5f0c3a9e1d2b4c6f8a7e9d0b1c2a3f4e5d6c7b8a9f0e1d2c3b4a5f6e7d8c9b0a";

/// The app: the key on this machine, and an application for namespaces.
pub struct App;

impl ResourceApp for App {
    type Resource = Publisher;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;

    fn scenarios() -> &'static [&'static str] {
        &["key"]
    }

    fn fixture(fixture: &mut Fixture<Publisher>) {
        if fixture.scenario() == Some("key") {
            fixture.provide(KeyState::Present {
                public: CAPTURE_PUBLIC_KEY.to_owned(),
            });
        }
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<Publisher>> {
        match name {
            Some("key") => vec![
                Publisher::SET_DOMAIN.with("example.com".into()),
                Publisher::SET_FORGE_ACCOUNT.with("github:ada".into()),
                Publisher::SET_CONTACT.with("ada@example.com".into()),
            ],
            _ => Vec::new(),
        }
    }

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
        let form = ui::column![
            Text::new("publisher-namespace-heading").with_role(TextRole::Heading),
            Text::new("publisher-namespace-help"),
            labeled_field("publisher-domain-label", domain),
            labeled_field("publisher-forge-label", forge),
            labeled_field("publisher-contact-label", contact),
            labeled_field("publisher-registry-label", registry),
        ];
        // The key is this machine's, not the version's: a past version shows
        // the application it held and nothing that acts on the key.
        let key = match cx.mode() {
            RunMode::Past => return Box::new(form),
            RunMode::Live => KeyState::load(),
            RunMode::Capture => cx
                .fixture::<KeyState>()
                .map_or(KeyState::Missing, |key| (*key).clone()),
        };
        let pane = cx.local(Pane {
            key,
            busy: false,
            notice: None,
        });
        let handler = pane.clone();
        cx.effects(move |effect: KeyEffect, cx| {
            if matches!(
                effect,
                KeyEffect::ApplyForNamespaces | KeyEffect::CheckStatus
            ) {
                let profile = cx.live().with(Clone::clone);
                handler.set(Pane {
                    busy: true,
                    notice: None,
                    ..handler.get()
                });
                let pane = handler.clone();
                return Ok(cx.thread(
                    move |_| remote(&effect, &profile),
                    move |(), result: Result<Notice, String>, _| {
                        pane.set(Pane {
                            key: KeyState::load(),
                            busy: false,
                            notice: Some(result.clone().unwrap_or_else(Notice::error)),
                        });
                        result.map(|_| Effected::done())
                    },
                ));
            }
            let result = local(&effect);
            handler.set(Pane {
                key: KeyState::load(),
                busy: false,
                notice: Some(result.clone().unwrap_or_else(Notice::error)),
            });
            result.map(|_| Effected::done())
        })
        .status({
            let pane = pane.clone();
            move || unavailable(&pane.get())
        });

        let actions = cx.actions();
        let apply_button = actions.bind_effect_invocation(
            Button::new("apply", "publisher-apply", None),
            &KeyEffect::ApplyForNamespaces,
        );
        let key =
            ModelView::from_state_by(&pane, Clone::clone, move |pane| key_section(pane, &actions));
        Box::new(ui::column![key, form, apply_button])
    }
}

ui::dev_reload_entry!(App);
