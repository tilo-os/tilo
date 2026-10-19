//! App Publisher: the key that signs your apps, and the namespaces the
//! registry lets it publish under.
//!
//! The key lives in this app's secrets on this device, where `tilo-app` finds
//! it too ([`registry_client::key`]). It never enters the resource: the
//! resource is synced to collaborators and kept in history, and holds only
//! what an application for a namespace needs.

/// The application id: also the app whose secrets `tilo-app` reads the
/// publisher key from.
pub const APP_ID: &str = registry_client::key::SECRETS_APP;
/// The window and launcher title.
pub const APP_TITLE: &str = "App Publisher";

pub mod locales;

use registry_client::{RegistryClient, key};
use registry_types::application::{NamespaceClaim, proof_token};
use ui::prelude::*;
use ui::secrets::SecretStore;
use ui::widget::{BannerKind, Field};

/// What to apply for, and where. Stored, synced and undoable; the key is not
/// part of it. Each field is edited at its path; the registry must stay an
/// address the client accepts, however it is changed.
#[derive(Model, Debug, Clone, PartialEq)]
#[model(check = Publisher::valid)]
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

#[resource(kind = "publisher.profile", label = "App Publisher")]
impl Publisher {}

impl Publisher {
    /// The registry is HTTPS or this machine.
    fn valid(&self) -> Result<(), Refusal> {
        RegistryClient::validate_url(&self.registry)
            .map(drop)
            .map_err(|error| Refusal::new(error.to_string()))
    }
}

/// The key on this machine, read from the app's secrets.
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
    fn load(secrets: &SecretStore) -> Self {
        match key::discover(secrets) {
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
#[effect(operation = "so.tilo.publisher.key", device_only)]
pub enum KeyEffect {
    /// Create publisher key.
    #[effect(privileged)]
    CreateKey,
    /// Copy namespace proof.
    CopyProof,
    /// Copy key for CI.
    #[effect(privileged)]
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

fn apply(profile: &Publisher, secrets: &SecretStore) -> Result<String, String> {
    let key = key::resolve(None, secrets).map_err(|error| format!("{error:#}"))?;
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

fn check_status(registry: &str, secrets: &SecretStore) -> Result<Option<String>, String> {
    let key = key::resolve(None, secrets).map_err(|error| format!("{error:#}"))?;
    let client = RegistryClient::new(registry).map_err(|error| format!("{error:#}"))?;
    let status = client
        .application(&key::public_hex(&key))
        .map_err(|error| format!("{error:#}"))?;
    Ok(status.as_ref().map(registry_client::status_summary))
}

/// How long a copied secret key stays on the clipboard: time to paste it into
/// a CI secret in a browser.
const KEY_ON_CLIPBOARD_FOR: std::time::Duration = std::time::Duration::from_secs(120);

/// Create or copy the key now, on the UI thread: the clipboard lives there.
fn local(effect: &KeyEffect, secrets: &SecretStore) -> Result<Notice, String> {
    match effect {
        KeyEffect::CreateKey => {
            let key = key::create(secrets).map_err(|error| format!("{error:#}"))?;
            Ok(Notice::info("publisher-key-created").detailed(key::public_hex(&key)))
        }
        KeyEffect::CopyProof => {
            let key = key::resolve(None, secrets).map_err(|error| format!("{error:#}"))?;
            let proof = proof_token(&key::public_hex(&key));
            ui::text_selection::clipboard_copy(&proof);
            Ok(Notice::info("publisher-proof-copied").detailed(proof))
        }
        KeyEffect::CopySecret | KeyEffect::ApplyForNamespaces | KeyEffect::CheckStatus => {
            Err(format!("`{}` runs as a job", effect.id()))
        }
    }
}

/// Start copying the secret key, on the UI thread: the clipboard lives there.
fn copy_secret(secrets: &SecretStore) -> Result<ui::clipboard::ClipboardCopy, String> {
    let key = key::resolve(None, secrets).map_err(|error| format!("{error:#}"))?;
    Ok(ui::try_clipboard_copy_secret(
        &hex::encode(key.to_bytes()),
        KEY_ON_CLIPBOARD_FOR,
    ))
}

/// What a finished secret-key copy says.
fn copied_secret(copied: bool) -> Result<Notice, String> {
    if copied {
        Ok(Notice::success("publisher-secret-copied").explained("publisher-secret-copied-detail"))
    } else {
        Err("Could not copy the key to the clipboard".into())
    }
}

/// Ask the registry, off the UI thread.
fn remote(
    effect: &KeyEffect,
    profile: &Publisher,
    secrets: &SecretStore,
) -> Result<Notice, String> {
    if *effect == KeyEffect::ApplyForNamespaces {
        return apply(profile, secrets)
            .map(|status| Notice::info("publisher-status").detailed(status));
    }
    check_status(&profile.registry, secrets).map(|status| match status {
        Some(status) => Notice::info("publisher-status").detailed(status),
        None => Notice::info("publisher-no-application").detailed(String::new()),
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
                "The key on this machine cannot be read",
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

fn value_group(label: &str, value: &str, id: &str, help: Option<&str>) -> Element {
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(Box::new(Text::new(label).with_role(TextRole::Caption)))
        .push(Box::new(
            Scrollable::new_horizontal()
                .fit_content_height()
                .always_visible()
                .with_semantic_key(format!("{id}-well"))
                .with_child(Box::new(
                    Text::new(value).with_id(id).with_role(TextRole::Code),
                )),
        ));
    if let Some(help) = help {
        column = column.push(Box::new(Text::new(help).with_role(TextRole::Caption)));
    }
    Box::new(column)
}
fn labeled_field(label: &str, input: impl ui::widget::FormControl + 'static) -> Field {
    Field::new(label, input)
}
fn key_section(pane: &Pane, actions: &Actions<Publisher>) -> Element {
    let button = |id: &str, label: &str, effect: &KeyEffect| {
        actions.bind_effect_invocation(Button::new(id, label), effect)
    };
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(Box::new(
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
        KeyState::Unreadable => {
            column = column.push(Box::new(
                Banner::error(say("publisher-key-unreadable-title"))
                    .with_message(say("publisher-key-unreadable")),
            ));
        }
    }
    Box::new(column)
}

/// The namespace claims, each input bound to its field.
fn namespace_form(cx: &ViewCx<Publisher>) -> Column {
    let paths = cx.paths();
    let registry = cx.input(paths.registry).with_id("registry-input");
    let domain = cx
        .input(paths.domain)
        .with_id("domain-input")
        .with_placeholder("publisher-domain-placeholder");
    let forge = cx
        .input(paths.forge_account)
        .with_id("forge-input")
        .with_placeholder("publisher-forge-placeholder");
    let contact = cx
        .input(paths.contact)
        .with_id("contact-input")
        .with_placeholder("publisher-contact-placeholder");
    // One form, so the four labels share a column; on the key section's
    // content edge, not indented under it.
    let fields = ui::widget::Form::new().section(
        ui::widget::FormSection::untitled()
            .field(labeled_field("publisher-domain-label", domain))
            .field(labeled_field("publisher-forge-label", forge))
            .field(labeled_field("publisher-contact-label", contact))
            .field(labeled_field("publisher-registry-label", registry)),
    );
    ui::column![
        Text::new("publisher-namespace-heading")
            .with_id("namespaces-heading")
            .with_role(TextRole::Heading),
        Text::new("publisher-namespace-help"),
        fields,
    ]
    .with_no_padding()
    .with_intrinsic_height()
}

/// The app: the key on this machine, and an application for namespaces.
pub struct App;

impl ResourceApp for App {
    type Resource = Publisher;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    fn scenarios() -> &'static [&'static str] {
        &["no-key", "unreadable", "applied", "failed", "copied"]
    }

    fn fixture(fixture: &mut ui::framework::Fixture<Publisher>) {
        let key = match fixture.scenario() {
            Some("no-key") => KeyState::Missing,
            Some("unreadable") => KeyState::Unreadable,
            _ => KeyState::Present {
                public: "a1".repeat(32),
            },
        };
        fixture.provide(key);
    }

    /// A capture never reads this machine's key file or settings: the claims
    /// are filled in here, the key and the last outcome in [`staged`].
    fn scenario(name: Option<&str>) -> Vec<Invoke<Publisher>> {
        if name == Some("no-key") {
            return Vec::new();
        }
        let paths = Publisher::paths();
        [
            paths.domain.replace("lovelace.dev"),
            paths.forge_account.replace("codeberg:ada"),
            paths.contact.replace("ada@lovelace.dev"),
        ]
        .iter()
        .filter_map(|edit| Invoke::edit(edit).ok())
        .collect()
    }

    fn view(cx: &mut ViewCx<Publisher>) -> Element {
        let form = namespace_form(cx);
        // The key is this machine's, not the version's: a past version shows
        // the application it held and nothing that acts on the key.
        let secrets = cx.storage().secrets();
        let key = match cx.mode() {
            RunMode::Past => return Box::new(form),
            RunMode::Live => KeyState::load(&secrets),
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
                let (remote_secrets, reload_secrets) = (secrets.clone(), secrets.clone());
                return Ok(cx.thread(
                    move |_| remote(&effect, &profile, &remote_secrets),
                    move |(), result: Result<Notice, String>, _| {
                        pane.set(Pane {
                            key: KeyState::load(&reload_secrets),
                            busy: false,
                            notice: Some(result.clone().unwrap_or_else(Notice::error)),
                        });
                        result.map(|_| Effected::done())
                    },
                ));
            }
            if matches!(effect, KeyEffect::CopySecret) {
                // The copy settles without the UI thread waiting on it; the
                // effect finishes once it has.
                let copy = copy_secret(&secrets)?;
                handler.set(Pane {
                    busy: true,
                    notice: None,
                    ..handler.get()
                });
                let pane = handler.clone();
                let reload_secrets = secrets.clone();
                return Ok(cx.thread(
                    move |_| copy.wait(),
                    move |(), copied: bool, _| {
                        let result = copied_secret(copied);
                        pane.set(Pane {
                            key: KeyState::load(&reload_secrets),
                            busy: false,
                            notice: Some(result.clone().unwrap_or_else(Notice::error)),
                        });
                        result.map(|_| Effected::done())
                    },
                ));
            }
            let result = local(&effect, &secrets);
            handler.set(Pane {
                key: KeyState::load(&secrets),
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
            Button::new("apply", "publisher-apply"),
            &KeyEffect::ApplyForNamespaces,
        );
        let key =
            ModelView::from_state_by(&pane, Clone::clone, move |pane| key_section(pane, &actions));
        let notice = ModelView::from_state_by(&pane, Clone::clone, |pane| {
            let mut column = Column::new().with_no_padding().with_intrinsic_height();
            if let Some(notice) = &pane.notice {
                let mut banner = Banner::new(notice.kind, &notice.title).with_id("notice");
                if let Some(message) = &notice.message {
                    banner = banner.with_message(message);
                }
                column = column.push(Box::new(banner));
            }
            Box::new(column)
        });
        Box::new(
            Scrollable::new_vertical()
                .fill_parent_height()
                .with_child(Box::new(
                    ui::column![key, form, apply_button, notice].with_intrinsic_height(),
                )),
        )
    }
}

ui::dev_reload_entry!(App);
