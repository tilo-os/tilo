#![cfg(unix)]

//! The App Store's device work, answered for `so.tilo.store` by name: what its sources offer, what
//! this computer has of it, and the changes a person asks for. The app keeps the records; this
//! reads the device and changes it, and every answer is JSON the app reads into its own records.
//!
//! Everything that changes what a workspace runs is the workspace's own action, carried out by
//! this computer's daemon (`app_cli::device`): a version is named on the workspace's tree and
//! every device follows it. What a device merely reads — its Flatpak installation, the catalogues
//! its registries and Flathub publish — is read here.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tilolang_tilo::engine::Provider;

/// The one app this device answers engine work for.
pub const APP: &str = "so.tilo.store";

/// Registers the App Store's engine with the session.
pub fn register() {
    tilolang_tilo::engine::register(APP, Arc::new(Store));
}

/// The App Store's engine.
pub struct Store;

impl Provider for Store {
    fn call(&self, op: &str, input: &str) -> Result<String, String> {
        call(op, input)
    }

    fn start(&self, op: &str, input: &str) -> Result<String, String> {
        jobs::start(op, input)
    }

    fn status(&self, job: &str) -> Result<String, String> {
        jobs::status(job)
    }

    fn cancel(&self, job: &str) -> bool {
        jobs::cancel(job)
    }
}

fn reads<T: for<'de> Deserialize<'de>>(input: &str) -> Result<T, String> {
    serde_json::from_str(input).map_err(|error| format!("the request does not read: {error}"))
}

fn writes<T: Serialize>(answer: &T) -> Result<String, String> {
    serde_json::to_string(answer).map_err(|error| format!("the answer does not write: {error}"))
}

/// One record as the JSON text an item holds: how a list of the app's records travels.
fn record<T: Serialize>(value: &T) -> Result<String, String> {
    writes(value)
}

// ---------------------------------------------------------------- what the sources offer

/// One app as the app reads it: the catalogue's own record.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is one fact a source states about an app: runnable, pinned, fetched, \
              a bot, awaiting its developer, updatable; a state machine would invent relations \
              between them the sources do not have"
)]
#[derive(Clone, Debug, Default, Serialize)]
struct Item {
    identity: String,
    name: String,
    summary: String,
    description: String,
    developer: String,
    license: String,
    categories: Vec<String>,
    icon: String,
    screenshots: Vec<String>,
    homepage: String,
    source_code: String,
    origin: String,
    registry: String,
    branch: String,
    user_managed: bool,
    trust: String,
    /// Each version as its own JSON text: a tilolang package reads a list of its own records
    /// one item at a time, never out of one JSON text.
    versions: Vec<String>,
    available: String,
    installed: String,
    pinned: bool,
    fetched: String,
    runnable: bool,
    processors: String,
    awaiting_developer: bool,
    featured: i64,
    updated: String,
    publisher: String,
    digest: String,
    bot: bool,
    bot_triggers: Vec<String>,
    bot_apps: Vec<String>,
    bot_connectors: Vec<String>,
    bot_appearance: bool,
    runtime: String,
    updatable: bool,
    trouble: String,
    /// The languages its source says it is written in.
    supported_languages: Vec<String>,
}

#[allow(
    clippy::struct_field_names,
    reason = "a version's own number is `version`, as every source and the app call it"
)]
#[derive(Clone, Debug, Default, Serialize)]
struct Version {
    version: String,
    released: String,
    root: String,
    surface: String,
    size: u64,
    runtime: String,
    download: String,
}

#[derive(Clone, Debug, Default, Serialize)]
struct CatalogueAnswer {
    items: Vec<String>,
    remotes: Vec<String>,
    status: String,
    trouble: String,
    flatpak: bool,
    flatpak_trouble: String,
    unanswered: Vec<String>,
    expired: Vec<String>,
}

/// The catalogue the app reads: the last read, or a fresh one. The app starts a read as a job and
/// then asks for its answer, which is the same one the job kept.
fn catalogue(force: bool) -> Result<String, String> {
    if !force && let Some(kept) = cache().lock().ok().and_then(|kept| kept.clone()) {
        return Ok(kept);
    }
    let answer = read_catalogue()?;
    let text = writes(&answer)?;
    if let Ok(mut kept) = cache().lock() {
        *kept = Some(text.clone());
    }
    Ok(text)
}

fn cache() -> &'static Mutex<Option<String>> {
    static CACHE: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

fn read_catalogue() -> Result<CatalogueAnswer, String> {
    let mut answer = CatalogueAnswer {
        status: "Store refreshed.".to_owned(),
        flatpak: false,
        ..CatalogueAnswer::default()
    };
    let mut items = Vec::new();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    // What a person's own list hides, and what this device's administrator or their workspace
    // hides, is hidden everywhere in the Store.
    let blocks = registry_types::BlockList::load();

    let installed = installed_rows();
    let listing = app_cli::offered();
    for unreachable in &listing.unreachable {
        answer.unanswered.push(unreachable.name.clone());
    }
    for offer in &listing.apps {
        if blocks.is_blocked(&offer.package_id) {
            continue;
        }
        seen.insert(offer.package_id.clone());
        items.push(record(&tilo_item(offer, &installed))?);
    }
    // A directory bot has no version to fetch on this computer: adding it saves its program as
    // the person's shortcut, which the workspace's host runs for them.
    for bot in &listing.bots {
        if blocks.is_blocked(&bot.package_id) || !seen.insert(bot.package_id.clone()) {
            continue;
        }
        items.push(record(&tilo_bot_item(bot))?);
    }

    let flatpak = package_actions::flatpak::Flatpak::from_env();
    let status = match flatpak.status() {
        Ok(status) => status,
        Err(error) => {
            answer.flatpak_trouble = error;
            package_actions::flatpak::Status::default()
        }
    };
    answer.flatpak = status.available;
    for remote in &status.remotes {
        answer.remotes.push(record(&remote_item(remote))?);
    }
    // A Flatpak this computer runs that no source lists is still one a person may open or remove.
    for app in &status.installed {
        if blocks.is_blocked(&app.app) || !seen.insert(app.app.clone()) {
            continue;
        }
        items.push(record(&installed_flatpak_item(app))?);
    }

    answer.items = items;
    Ok(answer)
}

#[derive(Default)]
struct Installed {
    version: BTreeMap<String, String>,
    pinned: BTreeMap<String, bool>,
}

fn installed_rows() -> Installed {
    let mut rows = Installed::default();
    if let Ok(listed) = package_store::list_installed() {
        for entry in listed {
            rows.version.insert(entry.id.clone(), entry.version.clone());
            rows.pinned.insert(entry.id, entry.pinned);
        }
    }
    rows
}

fn tilo_item(offer: &app_cli::CatalogueApp, installed: &Installed) -> Item {
    let version = offer.version.clone();
    let installed_version = installed
        .version
        .get(&offer.package_id)
        .cloned()
        .unwrap_or_default();
    let pinned = installed
        .pinned
        .get(&offer.package_id)
        .copied()
        .unwrap_or(false);
    let icon = offer
        .images
        .iter()
        .find(|image| image.kind == registry_types::RegistryImageKind::Icon)
        .map(|image| image.url.clone())
        .unwrap_or_default();
    let screenshots = offer
        .images
        .iter()
        .filter(|image| image.kind == registry_types::RegistryImageKind::Screenshot)
        .map(|image| image.url.clone())
        .collect();
    let runnable = offer.architecture.is_empty()
        || offer.architecture == std::env::consts::ARCH
        || offer.architecture == "noarch";
    let updatable = !installed_version.is_empty() && version_greater(&version, &installed_version);
    Item {
        identity: offer.package_id.clone(),
        name: offer.display_name.clone(),
        summary: if offer.tagline.is_empty() {
            offer.description.clone()
        } else {
            offer.tagline.clone()
        },
        description: offer.description.clone(),
        developer: if offer.package_id.starts_with("so.tilo.") {
            "Tilo".to_owned()
        } else {
            String::new()
        },
        license: String::new(),
        categories: offer.category.iter().cloned().collect(),
        icon,
        screenshots,
        homepage: String::new(),
        source_code: String::new(),
        origin: "tilo".to_owned(),
        registry: offer.registry.clone(),
        branch: "stable".to_owned(),
        user_managed: true,
        trust: "publisher_signed".to_owned(),
        versions: vec![
            record(&Version {
                version: version.clone(),
                released: String::new(),
                root: offer.tapp_root_hash_hex.clone(),
                surface: offer.tapp_surface_hex.clone(),
                size: offer.tapp_size_bytes,
                runtime: offer.runtime.clone().unwrap_or_default(),
                download: offer.download_url.clone(),
            })
            .unwrap_or_default(),
        ],
        available: version,
        fetched: installed_version.clone(),
        installed: installed_version,
        pinned,
        runnable,
        processors: offer.architecture.clone(),
        awaiting_developer: false,
        featured: 0,
        updated: String::new(),
        publisher: offer.publisher_pubkey_hex.clone(),
        digest: offer.tapp_blake3_hex.clone(),
        bot: false,
        bot_triggers: Vec::new(),
        bot_apps: Vec::new(),
        bot_connectors: Vec::new(),
        bot_appearance: false,
        runtime: offer.runtime.clone().unwrap_or_default(),
        updatable,
        trouble: if runnable {
            String::new()
        } else {
            format!(
                "Not available for this computer: built for {}.",
                offer.architecture
            )
        },
        supported_languages: offer.supported_languages.clone(),
    }
}

/// One directory bot as the Store reads it: what its program does, what it reaches, and whether
/// it may post under other names. There is no version to fetch on this computer: adding it saves
/// the program as the person's shortcut.
fn tilo_bot_item(bot: &app_cli::bots::CatalogueBot) -> Item {
    Item {
        identity: bot.package_id.clone(),
        name: bot.name.clone(),
        summary: if bot.tagline.is_empty() {
            bot.description.clone()
        } else {
            bot.tagline.clone()
        },
        description: bot.description.clone(),
        developer: if bot.package_id.starts_with("so.tilo.") {
            "Tilo".to_owned()
        } else {
            String::new()
        },
        icon: bot.icon_url.clone().unwrap_or_default(),
        origin: "tilo".to_owned(),
        registry: bot.registry_name.clone(),
        branch: "stable".to_owned(),
        user_managed: true,
        trust: "publisher_signed".to_owned(),
        versions: vec![
            record(&Version {
                version: bot.version.clone(),
                ..Version::default()
            })
            .unwrap_or_default(),
        ],
        available: bot.version.clone(),
        runnable: true,
        publisher: bot.publisher_pubkey_hex.clone(),
        bot: true,
        bot_triggers: bot.artifact.requires.triggers.clone(),
        bot_apps: bot.artifact.requires.apps.clone(),
        bot_connectors: bot
            .artifact
            .requires
            .connectors
            .iter()
            .map(|connector| connector.package_id.clone())
            .collect(),
        bot_appearance: bot.artifact.requires.appearance,
        ..Item::default()
    }
}

fn installed_flatpak_item(app: &package_actions::flatpak::Installed) -> Item {
    Item {
        identity: app.app.clone(),
        name: app.app.clone(),
        summary: "Installed Flatpak. Catalogue details are unavailable.".to_owned(),
        description: String::new(),
        origin: "flathub".to_owned(),
        registry: "Flathub".to_owned(),
        branch: app.branch.clone(),
        trust: "flathub".to_owned(),
        versions: vec![
            record(&Version {
                version: app.version.clone(),
                ..Version::default()
            })
            .unwrap_or_default(),
        ],
        available: app.version.clone(),
        installed: app.version.clone(),
        fetched: app.version.clone(),
        runnable: true,
        user_managed: true,
        updatable: false,
        trouble: String::new(),
        ..Item::default()
    }
}

fn remote_item(remote: &package_actions::flatpak::Remote) -> Item {
    Item {
        identity: remote.name.clone(),
        name: if remote.title.is_empty() {
            remote.name.clone()
        } else {
            remote.title.clone()
        },
        homepage: remote.url.clone(),
        origin: "remote".to_owned(),
        registry: "device".to_owned(),
        branch: if remote.disabled { "off" } else { "on" }.to_owned(),
        available: String::new(),
        user_managed: false,
        trust: "configured_remote".to_owned(),
        runnable: true,
        updatable: false,
        trouble: String::new(),
        ..Item::default()
    }
}

// ---------------------------------------------------------------- one app, and a page

/// One app as it stands now: `null` when no source offers it.
fn one(input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        identity: String,
    }
    let ask: Ask = reads(input)?;
    require_app_id(&ask.identity)?;
    let blocks = registry_types::BlockList::load();
    if blocks.is_blocked(&ask.identity) {
        return writes(&json!({ "item": Value::Null }));
    }
    let installed = installed_rows();
    let listing = app_cli::offered();
    if let Some(offer) = listing
        .apps
        .into_iter()
        .find(|offer| offer.package_id == ask.identity)
    {
        return writes(&json!({ "item": record(&tilo_item(&offer, &installed))? }));
    }
    if let Some(bot) = listing
        .bots
        .into_iter()
        .find(|bot| bot.package_id == ask.identity)
    {
        return writes(&json!({ "item": record(&tilo_bot_item(&bot))? }));
    }
    Ok(r#"{"item":null}"#.to_owned())
}

fn details(input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        identity: String,
    }
    let ask: Ask = reads(input)?;
    require_app_id(&ask.identity)?;
    let listing = app_cli::offered();
    if let Some(offer) = listing
        .apps
        .into_iter()
        .find(|offer| offer.package_id == ask.identity)
    {
        let installed = installed_rows();
        let item = tilo_item(&offer, &installed);
        return writes(&json!({
            "description": item.description,
            "developer": item.developer,
            "license": item.license,
            "homepage": item.homepage,
            "source_code": item.source_code,
            "size": offer.tapp_size_bytes,
            "installed_size": 0,
            "screenshots": item.screenshots,
            "permissions": [],
            "releases": [],
            "runtime": item.runtime,
            "runtime_registry": "",
            "digest": item.digest,
            "supported_languages": item.supported_languages,
        }));
    }
    // A directory bot has nothing a source says beyond its listing: what its program does and
    // reaches is on the item, and there is no version or screenshot to fetch.
    if let Some(bot) = listing
        .bots
        .into_iter()
        .find(|bot| bot.package_id == ask.identity)
    {
        let item = tilo_bot_item(&bot);
        return writes(&json!({
            "description": item.description,
            "developer": item.developer,
            "license": item.license,
            "homepage": item.homepage,
            "source_code": item.source_code,
            "size": 0,
            "installed_size": 0,
            "screenshots": item.screenshots,
            "permissions": [],
            "releases": [],
            "runtime": item.runtime,
            "runtime_registry": "",
            "digest": item.digest,
            "supported_languages": item.supported_languages,
        }));
    }
    // A Flathub app's page reads Flathub itself, in the app.
    writes(&json!({
        "description": "",
        "developer": "",
        "license": "",
        "homepage": "",
        "source_code": "",
        "size": 0,
        "installed_size": 0,
        "screenshots": [],
        "permissions": [],
        "releases": [],
        "runtime": "",
        "runtime_registry": "",
        "digest": "",
        "supported_languages": [],
    }))
}

fn browse(input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        name: String,
    }
    let ask: Ask = reads(input)?;
    let flatpak = package_actions::flatpak::Flatpak::from_env();
    let rows = flatpak.catalogue(&ask.name)?;
    let installed = installed_rows();
    let mut items = Vec::new();
    for line in rows.lines() {
        let columns: Vec<&str> = line.split('\t').collect();
        let Some(app) = columns.first().map(|id| id.trim()) else {
            continue;
        };
        if app.is_empty() {
            continue;
        }
        let mut item = Item {
            identity: app.to_owned(),
            name: columns
                .get(1)
                .map(|name| name.trim())
                .filter(|name| !name.is_empty())
                .unwrap_or(app)
                .to_owned(),
            summary: columns.get(2).map_or("", |text| text.trim()).to_owned(),
            origin: "remote".to_owned(),
            registry: ask.name.clone(),
            branch: columns
                .get(4)
                .map(|branch| branch.trim())
                .filter(|branch| !branch.is_empty())
                .unwrap_or("stable")
                .to_owned(),
            trust: "configured_remote".to_owned(),
            runnable: true,
            user_managed: true,
            updatable: false,
            trouble: String::new(),
            ..Item::default()
        };
        if let Some(version) = columns
            .get(3)
            .map(|version| version.trim())
            .filter(|v| !v.is_empty())
        {
            let text = record(&Version {
                version: version.to_owned(),
                ..Version::default()
            })?;
            item.versions.push(text);
            version.clone_into(&mut item.available);
        }
        if let Some(installed) = installed.version.get(app) {
            item.installed.clone_from(installed);
            item.fetched.clone_from(installed);
        }
        items.push(item);
    }
    writes(&json!({ "items": items.iter().map(record).collect::<Result<Vec<_>, _>>()? }))
}

fn remotes() -> Result<String, String> {
    let flatpak = package_actions::flatpak::Flatpak::from_env();
    match flatpak.status() {
        Ok(status) => Ok(writes(&json!({
            "remotes": status
                .remotes
                .iter()
                .map(|remote| record(&json!({
                    "name": remote.name,
                    "title": remote.title,
                    "url": remote.url,
                    "scope": "device",
                    "enabled": !remote.disabled,
                    "trouble": "",
                })))
                .collect::<Result<Vec<_>, _>>()?,
            "flatpak": status.available,
            "trouble": "",
        }))?),
        Err(error) => Ok(writes(&json!({
            "remotes": [],
            "flatpak": false,
            "trouble": error,
        }))?),
    }
}

/// The exact signed offer of a runtime, so its page says what installing it would run.
fn runtime(input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        runtime: String,
        #[serde(default)]
        registry: String,
    }
    let ask: Ask = reads(input)?;
    require_app_id(&ask.runtime)?;
    let listing = app_cli::all_offers();
    let found: Vec<_> = listing
        .apps
        .into_iter()
        .filter(|offer| offer.package_id == ask.runtime)
        .filter(|offer| ask.registry.is_empty() || offer.registry == ask.registry)
        .collect();
    match found.len() {
        1 => {}
        0 => return writes(&json!({ "item": Value::Null })),
        _ => {
            return writes(&json!({
                "item": Value::Null,
                "trouble": "More than one repository offers this runtime. Choose one repository before installing.",
            }));
        }
    }
    let installed = installed_rows();
    let offer = found
        .first()
        .ok_or_else(|| "No enabled repository offers this runtime.".to_owned())?;
    writes(&json!({ "item": record(&tilo_item(offer, &installed))? }))
}

// ---------------------------------------------------------------- package files

/// The bytes a handed file names, from the content store the app's record lends the window.
fn file_bytes(reference: &str) -> Result<Vec<u8>, String> {
    let hash = reference
        .strip_prefix("tilo.content:")
        .or_else(|| reference.strip_prefix("tilo-blob://"))
        .unwrap_or(reference);
    tilo_resource::content::read_hash(hash).map_err(|refusal| refusal.to_string())
}

/// Whether `a` names a newer version than `b`: dot-separated numbers, then more parts.
fn version_greater(a: &str, b: &str) -> bool {
    if b.is_empty() {
        return !a.is_empty();
    }
    let left: Vec<u64> = a.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    let right: Vec<u64> = b.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    for i in 0..left.len().max(right.len()) {
        let x = left.get(i).copied().unwrap_or(0);
        let y = right.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

fn require_app_id(app: &str) -> Result<(), String> {
    let ok = !app.is_empty()
        && app.len() <= 255
        && app != "."
        && app != ".."
        && app
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b':'));
    if ok {
        Ok(())
    } else {
        Err("that is not an app identity".to_owned())
    }
}

/// Reviews a package file a person handed the Store: what it is, and the exact root the workspace
/// checks before it installs anything.
fn review(input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        file: String,
    }
    let ask: Ask = reads(input)?;
    let bytes = file_bytes(&ask.file)?;
    if bytes.is_empty() || bytes.len() > MAX_IMPORT_BYTES {
        return review_trouble("Choose a Tilo app package smaller than 64 MiB.");
    }
    let mut discard = tilo_app_package::blobs::DiscardBlobs;
    let Ok(tapp) = tilo_app_package::tapp::read(bytes.as_slice(), &mut discard) else {
        return review_trouble("This file is not a valid signed Tilo package.");
    };
    let package = &tapp.meta.manifest.package;
    if !matches!(package.kind, tilo_app_package::Kind::App) {
        return review_trouble(
            "Choose an app package. System components and runtimes use the system updater.",
        );
    }
    if tapp.meta.root.signature.is_none() {
        return review_trouble("This package is not signed by a publisher.");
    }
    let root = hex::encode(tapp.meta.id.0);
    let publisher = hex::encode(tapp.meta.root.publisher_identity);
    let listing = app_cli::all_offers();
    let mut item = listing
        .apps
        .iter()
        .find(|offer| {
            offer.package_id == package.id
                && offer.tapp_root_hash_hex == root
                && offer.publisher_pubkey_hex == publisher
        })
        .map(|offer| {
            let mut item = tilo_item(offer, &installed_rows());
            item.versions.truncate(1);
            item
        });
    let local = item.is_none();
    let item = item.get_or_insert_with(|| Item {
        identity: package.id.clone(),
        name: package.name.clone().unwrap_or_else(|| package.id.clone()),
        summary: "Signed local application.".to_owned(),
        description: "Signed local application. Installation requires its payload and exact runtime to be available.".to_owned(),
        origin: "local".to_owned(),
        registry: "Local package".to_owned(),
        trust: "attestor".to_owned(),
        versions: vec![record(&Version {
            version: package.version.clone(),
            root: root.clone(),
            surface: String::new(),
            size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            ..Version::default()
        })
        .unwrap_or_default()],
        available: package.version.clone(),
        runnable: true,
        publisher: publisher.clone(),
        user_managed: true,
        updatable: false,
        trouble: String::new(),
        ..Item::default()
    });
    if local {
        "attestor".clone_into(&mut item.trust);
    }
    writes(&json!({
        "item": record(item)?,
        "root": root,
        "trouble": "",
    }))
}

fn review_trouble(why: &str) -> Result<String, String> {
    writes(&json!({ "trouble": why }))
}

// ---------------------------------------------------------------- changes

/// A repository the app asks this device to browse.
#[derive(Deserialize)]
struct BrowseAsk {
    name: String,
}

/// The most bytes a package a person hands the Store may be.
const MAX_IMPORT_BYTES: usize = 64 * 1024 * 1024;

/// One change an app asks this device for.
#[derive(Clone, Debug, Default, Deserialize)]
struct Change {
    identity: String,
    kind: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    root: String,
    #[serde(default)]
    surface: String,
    #[serde(default)]
    publisher: String,
    #[serde(default)]
    flatpak: String,
    #[serde(default)]
    file: String,
}

fn change(change: &Change, update: bool) -> Result<String, String> {
    require_app_id(&change.identity)?;
    match change.kind.as_str() {
        "tilo" => {
            // The exact signed version the person reviewed, when the app named one: a registry
            // that changed its offer since must not move what is installed.
            let release = if !change.root.is_empty() && !change.publisher.is_empty() {
                app_cli::device::release(
                    &change.identity,
                    &change.version,
                    &change.surface,
                    &change.root,
                    &change.publisher,
                )
            } else {
                let offer = app_cli::all_offers()
                    .apps
                    .into_iter()
                    .find(|offer| {
                        offer.package_id == change.identity
                            && (change.version.is_empty() || offer.version == change.version)
                    })
                    .ok_or_else(|| {
                        "No repository offers this version now. Refresh the catalogue.".to_owned()
                    })?;
                app_cli::device::release_of(&offer)
            };
            if update {
                app_cli::device::update_to(&change.identity, &release)
                    .map(|_| "App updated.".to_owned())
            } else {
                app_cli::device::install_offered(&release).map(|_| "App installed.".to_owned())
            }
        }
        "local" => {
            if change.file.is_empty() {
                return Err("Review this exact package before installing it.".to_owned());
            }
            let bytes = file_bytes(&change.file)?;
            let mut staged = tempfile::NamedTempFile::new()
                .map_err(|error| format!("no place to stage the package: {error}"))?;
            std::io::Write::write_all(&mut staged, &bytes)
                .map_err(|error| format!("the package cannot be staged: {error}"))?;
            app_cli::device::install_file(staged.path(), false, false)
                .map(|_| "Package installed. Open it from its app details.".to_owned())
        }
        "flatpak" => {
            let reference = flatpak_reference(&change.identity, &change.flatpak);
            let mut daemon = app_cli::device::Daemon::reach()?;
            daemon
                .call(
                    "tilo_do",
                    &json!({
                        "path": "workspace/apps#install",
                        "wait_ms": app_cli::device::CHANGE_WAIT_MS,
                        "args": {
                            "app": change.identity,
                            "version": change.flatpak,
                            "flatpak": reference,
                        },
                    }),
                )
                .map(|_| "App installed.".to_owned())
        }
        "install-runtime" => {
            let offer = app_cli::all_offers()
                .apps
                .into_iter()
                .find(|offer| offer.package_id == change.identity)
                .ok_or_else(|| {
                    "No enabled repository offers the required runtime for this computer."
                        .to_owned()
                })?;
            app_cli::device::install_offered(&app_cli::device::release_of(&offer))
                .map(|_| format!("Installed {}.", offer.display_name))
        }
        _ => Err("unsupported change".to_owned()),
    }
}

fn flatpak_reference(app: &str, branch: &str) -> String {
    let branch = if branch.is_empty() { "stable" } else { branch };
    format!("app/{app}/{}/{branch}", std::env::consts::ARCH)
}

fn uninstall(change: &Change) -> Result<String, String> {
    require_app_id(&change.identity)?;
    if change.kind == "flatpak" {
        return app_cli::device::on_app(&change.identity, "uninstall")
            .map(|_| "App removed.".to_owned());
    }
    app_cli::device::on_app(&change.identity, "uninstall").map(|_| "App removed.".to_owned())
}

fn pin(change: &Change, pinned: bool) -> Result<String, String> {
    require_app_id(&change.identity)?;
    app_cli::device::on_app(&change.identity, if pinned { "pin" } else { "unpin" }).map(|_| {
        if pinned {
            "Kept on this device.".to_owned()
        } else {
            "Allowed to update.".to_owned()
        }
    })
}

fn update_all(input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        list: Vec<Change>,
    }
    let ask: Ask = reads(input)?;
    let total = ask.list.len();
    let mut updated: usize = 0;
    let mut first = String::new();
    for change in &ask.list {
        match change_app_to_newest(change) {
            Ok(()) => updated = updated.saturating_add(1),
            Err(error) => {
                if first.is_empty() {
                    first = error;
                }
            }
        }
    }
    if updated == total {
        Ok(format!("Updated {total} apps."))
    } else if first.is_empty() {
        Ok(format!("Updated {updated} of {total} apps."))
    } else {
        Ok(format!("Updated {updated} of {total} apps; {first}"))
    }
}

fn change_app_to_newest(change: &Change) -> Result<(), String> {
    let offer = app_cli::all_offers()
        .apps
        .into_iter()
        .find(|offer| offer.package_id == change.identity)
        .ok_or_else(|| format!("no repository offers {}", change.identity))?;
    app_cli::device::update_to(&change.identity, &app_cli::device::release_of(&offer))
        .map(|_| ())
        .map_err(|error| format!("{}: {error}", change.identity))
}

// ---------------------------------------------------------------- repositories

fn remote_change(op: &str, input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        name: String,
        #[serde(default)]
        url: String,
        #[serde(default)]
        enabled: bool,
    }
    let ask: Ask = reads(input)?;
    let flatpak = package_actions::flatpak::Flatpak::from_env();
    match op {
        "remote-add" => flatpak.remote_add(&ask.name, &ask.url)?,
        "remote-enable" => flatpak.remote_set_enabled(&ask.name, ask.enabled)?,
        "remote-remove" => flatpak.remote_remove(&ask.name)?,
        _ => return Err("unsupported repository change".to_owned()),
    }
    Ok("Repository updated.".to_owned())
}

// ---------------------------------------------------------------- reports

fn registry_of(app: &str, wanted: &str) -> Result<app_cli::RegistryConfig, String> {
    let name = if wanted.is_empty() {
        let offered = app_cli::offered();
        offered
            .apps
            .into_iter()
            .find(|offer| offer.package_id == app)
            .map(|offer| offer.registry)
            .or_else(|| {
                offered
                    .bots
                    .into_iter()
                    .find(|bot| bot.package_id == app)
                    .map(|bot| bot.registry_name)
            })
    } else {
        Some(wanted.to_owned())
    }
    .ok_or_else(|| "The app's registry is unavailable.".to_owned())?;
    app_cli::config::load_registries()
        .registries
        .into_iter()
        .find(|registry| registry.enabled && registry.name == name)
        .ok_or_else(|| "The app's registry is unavailable.".to_owned())
}

// ---------------------------------------------------------------- directory bots

/// Add a directory bot's program as the person's saved shortcut: it runs through the same trigger
/// runner as any automation, and the workspace's host runs it for them when their own devices are
/// away. A program given as text — a developer's own, or one a file holds — is checked and saved
/// the same way, with no registry and no version to fetch.
fn add_bot(input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        #[serde(default)]
        identity: String,
        #[serde(default)]
        registry: String,
        #[serde(default)]
        program: String,
    }
    let ask: Ask = reads(input)?;
    let program = if ask.program.trim().is_empty() {
        require_app_id(&ask.identity)?;
        let registry = registry_of(&ask.identity, &ask.registry)?;
        tilo_bot_directory::fetch_listed_bot(&registry, &ask.identity, tilo_bot_directory::NEWEST)
            .map_err(|error| error.to_string())?
            .program
    } else {
        let program = tilo_automation::program::parse_program(ask.program.as_bytes())
            .map_err(|error| error.to_string())?;
        tilo_automation::program::check_structure(&program).map_err(|errors| {
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        })?;
        program
    };
    let name = program.name.clone();
    // The program's own id names the shortcut, so adding a newer program of the same bot replaces
    // what runs instead of saving a second copy beside it.
    let id = program.id.clone();
    let mut daemon = app_cli::device::Daemon::reach()?;
    let saved = daemon.workspace("workspace/shortcuts#list", &json!({}))?;
    let replacing = saved
        .as_array()
        .is_some_and(|list| list.iter().any(|one| one["id"] == json!(id)));
    let action = if replacing { "update" } else { "create" };
    daemon.workspace(
        &format!("workspace/shortcuts#{action}"),
        &json!({ "shortcut": program }),
    )?;
    Ok(format!(
        "“{name}” is saved in your shortcuts; the workspace's host runs it for you."
    ))
}

fn prepare_report(input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        identity: String,
        #[serde(default)]
        note: String,
        #[serde(default)]
        case: String,
        #[serde(default)]
        registry: String,
        appeal: bool,
    }
    let ask: Ask = reads(input)?;
    require_app_id(&ask.identity)?;
    let registry = registry_of(&ask.identity, &ask.registry)?;
    let now = unix_now();
    let mutation = if ask.appeal {
        registry_types::ModerationMutation::PublisherAppeal {
            case_id: ask.case.clone(),
            reason: ask.note.clone(),
            evidence_digest_hex: None,
        }
    } else {
        if !ask.case.is_empty() {
            return Err("this report was already submitted".to_owned());
        }
        let evidence =
            registry_types::report_evidence_digest(&ask.identity, "Not submitted", &ask.note, None)
                .map_err(|error| error.to_string())?;
        registry_types::ModerationMutation::FileReport {
            package_id: ask.identity.clone(),
            category: "policy".to_owned(),
            summary: ask.note.trim().to_owned(),
            evidence_digest_hex: evidence,
        }
    };
    let mut nonce = [0_u8; 16];
    getrandom::fill(&mut nonce).map_err(|error| error.to_string())?;
    let operation = registry_types::ExternalModerationOperation {
        destination: registry_types::ModerationDestination {
            base_url: registry.base_url.clone(),
            owner_pubkey_hex: registry.owner_pubkey_hex.clone(),
        },
        request_id: request_id(&ask.identity, &ask.note, now, &nonce),
        issued_at_unix: now,
        expires_at_unix: now.saturating_add(3600),
        mutation,
    };
    operation.validate()?;
    let operation = serde_json::to_string(&operation)
        .map_err(|error| format!("the report does not write: {error}"))?;
    writes(&json!({
        "operation": operation,
        "registry": registry.name,
        "trouble": "",
    }))
}

fn report_status(input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        identity: String,
        case: String,
        #[serde(default)]
        registry: String,
    }
    let ask: Ask = reads(input)?;
    let registry = registry_of(&ask.identity, &ask.registry)?;
    match verified_case(&registry, &ask.case) {
        Ok(case) => writes(&json!({
            "case": case.case_id,
            "status": status_words(case.status),
            "situation": "",
            "trouble": "",
        })),
        Err(error) => writes(&json!({
            "case": ask.case,
            "status": "",
            "situation": "",
            "trouble": error,
        })),
    }
}

fn status_words(status: registry_types::ModerationCaseStatus) -> &'static str {
    match status {
        registry_types::ModerationCaseStatus::Open => "Open",
        registry_types::ModerationCaseStatus::NeedsInformation => "Needs information",
        registry_types::ModerationCaseStatus::Appealed => "Appealed",
        registry_types::ModerationCaseStatus::ClosedNoAction => "Closed, no action",
        registry_types::ModerationCaseStatus::Quarantined => "Quarantined",
        registry_types::ModerationCaseStatus::Restored => "Restored",
    }
}

/// The registry's verified status for a case: its owner-signed transparency log, not a mutable
/// server projection.
fn verified_case(
    registry: &app_cli::RegistryConfig,
    case_id: &str,
) -> Result<registry_types::ModerationCase, String> {
    let valid = !case_id.is_empty()
        && case_id.len() <= 96
        && case_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'));
    if !valid {
        return Err("invalid case id".to_owned());
    }
    let snapshot = app_cli::client::fetch_snapshot(registry).map_err(|error| error.to_string())?;
    let mut events = Vec::new();
    let mut from = 1_u64;
    loop {
        let page = transparency(&registry.base_url, from)?;
        if page.events.is_empty() {
            break;
        }
        events.extend(page.events);
        if events.len() > 10_000 {
            return Err("the registry returned an invalid moderation response".to_owned());
        }
        match page.next_sequence {
            Some(next) if next > from => from = next,
            Some(_) => {
                return Err("the registry returned an invalid moderation response".to_owned());
            }
            None => break,
        }
    }
    registry_types::derive_verified_case(
        &events,
        case_id,
        &registry.owner_pubkey_hex,
        snapshot.moderation_head_hex(),
    )
    .map_err(|error| error.to_string())
}

fn transparency(base_url: &str, from: u64) -> Result<registry_types::TransparencyPage, String> {
    let mut url = moderation_url(base_url, "moderation/transparency")?;
    url.query_pairs_mut()
        .append_pair("from", &from.max(1).to_string())
        .append_pair("limit", "100");
    let client = tilo_net::http::blocking_client()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|error| format!("http client: {error}"))?;
    let text = client
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .and_then(reqwest::blocking::Response::text)
        .map_err(|error| format!("registry moderation service is offline: {error}"))?;
    serde_json::from_str(&text)
        .map_err(|_| "the registry returned an invalid moderation response".to_owned())
}

fn moderation_url(base_url: &str, suffix: &str) -> Result<reqwest::Url, String> {
    let mut url =
        reqwest::Url::parse(base_url).map_err(|_| "invalid registry address".to_owned())?;
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return Err("registry URL must be credential-free HTTPS".to_owned());
    }
    url.set_fragment(None);
    url.set_query(None);
    url.set_path(&format!("{}/{suffix}", url.path().trim_end_matches('/')));
    Ok(url)
}

fn request_id(package_id: &str, note: &str, now: u64, nonce: &[u8; 16]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(package_id.as_bytes());
    hasher.update(&[0]);
    hasher.update(note.as_bytes());
    hasher.update(&now.to_le_bytes());
    hasher.update(nonce);
    let digest = hasher.finalize();
    format!(
        "store-{}",
        digest.to_hex().chars().take(32).collect::<String>()
    )
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

// ---------------------------------------------------------------- the small answers

fn block(input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        identity: String,
        blocked: bool,
    }
    let ask: Ask = reads(input)?;
    require_app_id(&ask.identity)?;
    let path = registry_types::block::person_list_path()
        .ok_or_else(|| "Your block list could not be saved. Try again.".to_owned())?;
    let mut blocks = registry_types::BlockList::load();
    if ask.blocked {
        blocks.block(&ask.identity);
    } else {
        blocks
            .unblock(&ask.identity, &registry_types::Blocker::Person)
            .map_err(|error| match error {
                registry_types::UnblockError::NotBlocked => "This app is not blocked.".to_owned(),
                registry_types::UnblockError::NotTheBlocker(_) => {
                    "Only whoever blocked this app can unblock it.".to_owned()
                }
            })?;
    }
    blocks
        .save_person(&path)
        .map_err(|_| "Your block list could not be saved. Try again.".to_owned())?;
    Ok("{}".to_owned())
}

// ---------------------------------------------------------------- jobs

struct Job {
    state: Mutex<JobState>,
    cancel: AtomicBool,
}

#[derive(Clone, Debug, Default)]
struct JobState {
    state: String,
    done: u64,
    total: u64,
    error: String,
    status: String,
}

impl JobState {
    fn json(&self) -> Value {
        json!({
            "state": self.state,
            "done": self.done,
            "total": self.total,
            "error": self.error,
            "status": self.status,
        })
    }
}

mod jobs {
    use super::{
        Arc, AtomicBool, AtomicU64, Job, JobState, Mutex, OnceLock, Ordering, change, json,
        prepare_report, report_status, uninstall, update_all,
    };
    use std::collections::BTreeMap;

    fn table() -> &'static Mutex<BTreeMap<String, Arc<Job>>> {
        static JOBS: OnceLock<Mutex<BTreeMap<String, Arc<Job>>>> = OnceLock::new();
        JOBS.get_or_init(|| Mutex::new(BTreeMap::new()))
    }

    fn next() -> String {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        format!("store-{}", NEXT.fetch_add(1, Ordering::Relaxed))
    }

    fn started(job: &str) -> String {
        json!({ "job": job, "error": "" }).to_string()
    }

    /// Runs `work` after the answer, keeping what it came to.
    fn spawn(
        op: &'static str,
        work: impl FnOnce(&AtomicBool, &mut dyn FnMut(u64, u64)) -> Result<String, String>
        + Send
        + 'static,
    ) -> String {
        let id = next();
        let job = Arc::new(Job {
            state: Mutex::new(JobState {
                state: "running".to_owned(),
                ..JobState::default()
            }),
            cancel: AtomicBool::new(false),
        });
        if let Ok(mut known) = table().lock() {
            known.insert(id.clone(), Arc::clone(&job));
        }
        let running = Arc::clone(&job);
        let spawned = std::thread::Builder::new()
            .name(format!("store-{op}"))
            .spawn(move || {
                let cancel = Arc::clone(&running);
                let mut progress = |done: u64, total: u64| {
                    if let Ok(mut state) = running.state.lock() {
                        state.done = done;
                        state.total = total;
                    }
                };
                let outcome = work(&cancel.cancel, &mut progress);
                if let Ok(mut state) = running.state.lock() {
                    if cancel.cancel.load(Ordering::Relaxed) {
                        "cancelled".clone_into(&mut state.state);
                    } else {
                        match outcome {
                            Ok(status) => {
                                "done".clone_into(&mut state.state);
                                state.status = status;
                            }
                            Err(error) => {
                                "failed".clone_into(&mut state.state);
                                state.error = error;
                            }
                        }
                    }
                }
            });
        if let Err(error) = spawned {
            return json!({ "job": "", "error": error.to_string() }).to_string();
        }
        started(&id)
    }

    pub(super) fn start(op: &str, input: &str) -> Result<String, String> {
        let input = input.to_owned();
        let op = op.to_owned();
        match op.as_str() {
            "catalogue" => Ok(spawn("catalogue", |cancel, progress| {
                if cancel.load(Ordering::Relaxed) {
                    return Ok(String::new());
                }
                progress(0, 1);
                super::catalogue(true)?;
                progress(1, 1);
                Ok("Store refreshed.".to_owned())
            })),
            "install" | "update" | "install-runtime" => {
                let asked: super::Change = super::reads(&input)?;
                let update = op == "update";
                Ok(spawn("change", move |cancel, _progress| {
                    if cancel.load(Ordering::Relaxed) {
                        return Ok(String::new());
                    }
                    change(&asked, update)
                }))
            }
            "uninstall" => {
                let asked: super::Change = super::reads(&input)?;
                Ok(spawn("uninstall", move |cancel, _| {
                    if cancel.load(Ordering::Relaxed) {
                        return Ok(String::new());
                    }
                    uninstall(&asked)
                }))
            }
            "pin" | "unpin" => {
                let asked: super::Change = super::reads(&input)?;
                let pinned = op == "pin";
                Ok(spawn("pin", move |cancel, _| {
                    if cancel.load(Ordering::Relaxed) {
                        return Ok(String::new());
                    }
                    super::pin(&asked, pinned)
                }))
            }
            "update-all" => Ok(spawn("update-all", move |_cancel, progress| {
                #[derive(serde::Deserialize)]
                struct Ask {
                    list: Vec<super::Change>,
                }
                let ask: Ask = super::reads(&input)?;
                let total = u64::try_from(ask.list.len()).unwrap_or(u64::MAX);
                progress(0, total);
                let status = update_all(&input)?;
                progress(total, total);
                Ok(status)
            })),
            "remote-add" | "remote-enable" | "remote-remove" => {
                let op = op.clone();
                Ok(spawn("remote", move |cancel, _| {
                    if cancel.load(Ordering::Relaxed) {
                        return Ok(String::new());
                    }
                    super::remote_change(&op, &input)
                }))
            }
            "browse" => Ok(spawn("browse", move |cancel, _| {
                if cancel.load(Ordering::Relaxed) {
                    return Ok(String::new());
                }
                let ask: super::BrowseAsk = super::reads(&input)?;
                super::browse(&json!({ "name": ask.name }).to_string())
            })),
            "prepare-report" => Ok(spawn("prepare-report", move |_, _| {
                prepare_report(&input)
            })),
            "report-status" => Ok(spawn("report-status", move |_, _| {
                report_status(&input)
            })),
            "add-bot" => Ok(spawn("add-bot", move |cancel, _| {
                if cancel.load(Ordering::Relaxed) {
                    return Ok(String::new());
                }
                super::add_bot(&input)
            })),
            "submit-report" | "appeal" => Ok(json!({
                "job": "",
                "error": "A report is signed with your workspace identity, which this computer cannot do from here yet.",
            })
            .to_string()),
            other => Err(format!("`{other}` is no job this device runs")),
        }
    }

    pub(super) fn status(job: &str) -> Result<String, String> {
        let found = {
            let known = table()
                .lock()
                .map_err(|_| "the job table is unreadable".to_owned())?;
            known.get(job).cloned()
        };
        let Some(found) = found else {
            return Err(format!("no job `{job}` runs on this device"));
        };
        let state = found
            .state
            .lock()
            .map_err(|_| "the job is unreadable".to_owned())?;
        Ok(state.json().to_string())
    }

    pub(super) fn cancel(job: &str) -> bool {
        let Ok(known) = table().lock() else {
            return false;
        };
        let Some(job) = known.get(job) else {
            return false;
        };
        if let Ok(state) = job.state.lock()
            && matches!(state.state.as_str(), "done" | "failed" | "cancelled")
        {
            return false;
        }
        job.cancel.store(true, Ordering::Relaxed);
        true
    }
}

// ---------------------------------------------------------------- the one dispatch

fn call(op: &str, input: &str) -> Result<String, String> {
    match op {
        "catalogue" => catalogue(false),
        "one" => one(input),
        "details" => details(input),
        "remotes" => remotes(),
        "browse" => browse(input),
        "runtime" => runtime(input),
        "review" => review(input),
        "reference" => reference(input),
        "prepare-report" => prepare_report(input),
        "report-status" => report_status(input),
        "add-bot" => add_bot(input),
        "block" => block(input),
        other => Err(format!("`{other}` is no op this device runs for {APP}")),
    }
}

/// Reads a `.flatpakref` a person opened: it joins the catalogue as a card, and installing it
/// stays a separate step.
fn reference(input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        file: String,
    }
    let ask: Ask = reads(input)?;
    let bytes = file_bytes(&ask.file)?;
    let Ok(contents) = std::str::from_utf8(&bytes) else {
        return writes(&json!({ "trouble": "The opened reference is not text." }));
    };
    let Some(found) = flatpak_ref(contents) else {
        return writes(&json!({ "trouble": "This file is not a Flatpak reference." }));
    };
    let installed = installed_rows();
    let item = Item {
        identity: found.app_id.clone(),
        name: if found.title.is_empty() {
            found.app_id.clone()
        } else {
            found.title.clone()
        },
        summary: found.comment.clone(),
        description: String::new(),
        origin: "flathub".to_owned(),
        registry: "Flathub".to_owned(),
        branch: found.branch.clone(),
        trust: "flathub".to_owned(),
        versions: vec![
            record(&Version {
                version: found.branch.clone(),
                ..Version::default()
            })
            .unwrap_or_default(),
        ],
        available: found.branch.clone(),
        installed: installed
            .version
            .get(&found.app_id)
            .cloned()
            .unwrap_or_default(),
        fetched: installed
            .version
            .get(&found.app_id)
            .cloned()
            .unwrap_or_default(),
        runnable: true,
        homepage: found.url.clone(),
        publisher: String::new(),
        user_managed: true,
        updatable: false,
        trouble: String::new(),
        ..Item::default()
    };
    writes(&json!({
        "item": record(&item)?,
        "root": found.url,
        "trouble": "",
    }))
}

#[derive(Debug)]
struct FlatpakRef {
    app_id: String,
    title: String,
    comment: String,
    url: String,
    branch: String,
}

fn flatpak_ref(content: &str) -> Option<FlatpakRef> {
    let mut in_ref = false;
    let mut app_id = None;
    let mut title = String::new();
    let mut comment = String::new();
    let mut url = String::new();
    let mut branch = String::new();
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            in_ref = line.eq_ignore_ascii_case("[Flatpak Ref]");
            continue;
        }
        if !in_ref {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().to_owned();
        match key.trim() {
            "Name" if !value.is_empty() => app_id = Some(value),
            "Title" => title = value,
            "Comment" => comment = value,
            "Url" => url = value,
            "Branch" => branch = value,
            _ => {}
        }
    }
    let app_id = app_id?;
    if title.is_empty() {
        title.clone_from(&app_id);
    }
    if branch.is_empty() {
        "stable".clone_into(&mut branch);
    }
    Some(FlatpakRef {
        app_id,
        title,
        comment,
        url,
        branch,
    })
}

#[cfg(test)]
mod bot_tests {
    use super::*;

    /// What the Bots view reads of a listed bot: what its program does and reaches, where it
    /// comes from, and that it has no version to fetch on this computer.
    #[test]
    fn a_listed_bot_carries_what_its_program_does_and_reaches() {
        let bot = app_cli::bots::CatalogueBot {
            package_id: "so.tilo.bots.echo".into(),
            name: "Echo".into(),
            tagline: "Says what it is told".into(),
            description: "A program that posts on a schedule.".into(),
            icon_url: Some("https://example.test/echo.png".into()),
            publisher_pubkey_hex: "ab".repeat(32),
            registry_name: "Tilo main".into(),
            version: "1.2.0".into(),
            artifact: registry_types::BotArtifact {
                program_url: "https://example.test/echo.program".into(),
                program_blake3_hex: "00".repeat(32),
                program_size_bytes: 64,
                requires: registry_types::BotRequirements {
                    connectors: Vec::new(),
                    apps: vec!["so.tilo.chat".into()],
                    appearance: true,
                    triggers: vec!["schedule".into(), "resource_changed".into()],
                },
            },
        };
        let item = tilo_bot_item(&bot);
        assert!(item.bot, "the Store filters it among bots");
        assert_eq!(item.identity, "so.tilo.bots.echo");
        assert_eq!(item.registry, "Tilo main");
        assert_eq!(item.available, "1.2.0");
        assert_eq!(item.bot_triggers, ["schedule", "resource_changed"]);
        assert_eq!(item.bot_apps, ["so.tilo.chat"]);
        assert!(item.bot_appearance, "it may post under other names");
        assert!(
            item.installed.is_empty(),
            "nothing is fetched on this computer"
        );
        assert!(item.runnable, "a bot is always added, never built here");
    }
}
