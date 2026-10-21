//! Personal default applications: which installed app opens each kind of
//! file and link for this person, wherever they sign in. The choices are the
//! `default_apps` section of the person's settings, which the launcher reads
//! when it opens a file or a link. Workspace policy deliberately does not live
//! here: it is contextual and belongs to the shell's Workspace Settings.
//!
//! The page is organised by the one taxonomy of file types
//! ([`TypeCategory`], shared with Files, search and the Store): one row per
//! category that opens to the category's types. The category's app applies
//! to every type that has no choice of its own; a type's own choice is
//! marked "Changed" with a reset. Categories are ordered by how much the
//! person uses them; a category with no app and no files stays closed at
//! the bottom.

use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeMap, BTreeSet};

use desktop_policy::usage::TypeUsage;
use desktop_policy::{
    AppCandidate, DefaultAppResolver, FileType, PersonalDefaults, ResourceKind, TypeCategory,
};
use serde::{Deserialize, Serialize};
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, Effected, LiveRow};
use ui::i18n::ArgValue;
use ui::icons::Icon;
use ui::tilo_actions::ResourcePresentation;
use ui::tilo_resource::{self, Effect};
use ui::widget::{
    AppModel, Button, ButtonRole, ControlSize, DisclosureGroup, DisclosureRow, Dropdown, ListRow,
    ModelPath, Row, Section, Widget,
};

use crate::effects::{SettingsEffect, effect};
use crate::host::{Followed, PaneCx};
use crate::i18n::{t, t_args, type_category};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{compact_control, note, row, rows, section, setting_row_described, untitled};

crate::effects::effect_family!(DefaultAppsEffect => answer);

crate::area::pane_area! {
    DefaultAppsArea;

    fn reveal(&self, anchor: &str) -> Option<crate::area::Reveal> {
        self.reveal_type(anchor)
    }
}

/// The window's [`DefaultAppsArea`].
pub fn area(app: &crate::settings_app::SettingsApp) -> Option<&DefaultAppsArea> {
    app.areas().find()
}

/// The most types of file and link one category lists.
const MAX_KIND_ROWS: usize = 256;
/// The most apps one type lists.
const MAX_APP_ROWS: usize = 128;

/// The person's default apps, as their settings hold them.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
pub struct DefaultApps {
    /// The app each type of file or link opens with, over its category's,
    /// by the type's id (`ext:pdf`, `mime:text/html`, `scheme:https`).
    #[serde(default)]
    #[model(readonly)]
    pub handlers: BTreeMap<String, String>,
    /// The app every type of a category opens with unless the type has a
    /// handler of its own, by the category's id (`code`, `documents`).
    #[serde(default)]
    #[model(readonly)]
    pub categories: BTreeMap<String, String>,
}

impl ui::storage::SettingsSchema for DefaultApps {
    const SCOPE: ui::storage::Scope =
        ui::storage::Scope::Shared(desktop_policy::PERSONAL_DEFAULTS_SECTION);
}

#[ui::tilo_resource::resource(kind = "settings.default-apps", label = "Default apps")]
impl DefaultApps {}

impl DefaultApps {
    fn personal(&self) -> PersonalDefaults {
        PersonalDefaults {
            handlers: self
                .handlers
                .iter()
                .filter_map(|(kind, app)| Some((stored_kind(kind)?, app.clone())))
                .collect(),
            categories: self.categories.clone(),
        }
    }
}

/// Which app opens each type of file and link.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum DefaultAppsEffect {
    /// Open a type of file or link with an installed app, over its
    /// category's app.
    #[effect(idempotent)]
    SetDefaultApp {
        #[param("The type of file or link: `ext:pdf`, `mime:text/html`, `scheme:https`")]
        kind: String,
        #[param("The installed app's id")]
        app: String,
    },
    /// Open every type of a category with an installed app, except the
    /// types with an app of their own.
    #[effect(idempotent)]
    SetCategoryApp {
        #[param("The category: `documents`, `images`, `code`, `calendar-contacts`, …")]
        category: String,
        #[param("The installed app's id")]
        app: String,
    },
    /// Let a type of file or link open with its category's app again.
    #[effect(idempotent)]
    ResetDefaultApp {
        #[param("The type of file or link: `ext:pdf`, `scheme:https`")]
        kind: String,
    },
    /// Open the workspace's own app defaults in Workspace Settings.
    #[effect(job)]
    OpenWorkspaceDefaults,
}

/// Answer `effect`: only an installed app that claims the type (or
/// something of the category) is accepted; the shell is asked to open
/// Workspace Settings off the UI thread.
pub(crate) fn answer(
    app: &SettingsApp,
    effect: DefaultAppsEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    let area = || crate::panes::personal_apps::area(app).ok_or_else(crate::area::missing);
    match effect {
        DefaultAppsEffect::SetDefaultApp { kind, app: package } => {
            let file_type = FileType::of(parse_kind(&kind).ok_or_else(|| not_a_type(&kind))?);
            let area = area()?;
            let resolver = DefaultAppResolver::new(area.candidates());
            let handlers = file_type
                .choose(&package, &resolver)
                .map_err(|error| error.to_string())?;
            // Change only the chosen type in the latest settings, not a
            // cached snapshot that may omit newer or unrecognized handlers.
            app.host()
                .update(|stored: &mut DefaultApps| apply(&mut stored.handlers, handlers))
        }
        DefaultAppsEffect::SetCategoryApp {
            category,
            app: package,
        } => {
            let category = TypeCategory::from_slug(&category)
                .ok_or_else(|| format!("`{category}` is not a category of file types"))?;
            let area = area()?;
            DefaultAppResolver::new(area.candidates())
                .validate_category_app(category, &package)
                .map_err(|error| error.to_string())?;
            app.host().update(|stored: &mut DefaultApps| {
                stored
                    .categories
                    .insert(category.slug().to_owned(), package);
            })
        }
        DefaultAppsEffect::ResetDefaultApp { kind } => {
            let file_type = FileType::of(parse_kind(&kind).ok_or_else(|| not_a_type(&kind))?);
            app.host().update(|stored: &mut DefaultApps| {
                apply(&mut stored.handlers, file_type.reset());
            })
        }
        DefaultAppsEffect::OpenWorkspaceDefaults => {
            if !app.host().reaches_devices() {
                return Err("only a window someone uses opens workspace settings".into());
            }
            Ok(crate::device_read::job(
                cx,
                open_workspace_defaults,
                |_: &mut SettingsApp, opened: Result<(), String>, _| {
                    opened.map(|()| Effected::done())
                },
            ))
        }
    }
}

fn not_a_type(kind: &str) -> String {
    format!("`{kind}` is not a kind of file or link")
}

/// Set or drop each handler `changes` names, by the kind's id.
fn apply(handlers: &mut BTreeMap<String, String>, changes: Vec<(ResourceKind, Option<String>)>) {
    for (kind, app) in changes {
        match app {
            Some(app) => {
                handlers.insert(kind.as_str().to_owned(), app);
            }
            None => {
                handlers.remove(kind.as_str());
            }
        }
    }
}

/// The kind a stored handler names.
fn stored_kind(kind: &str) -> Option<ResourceKind> {
    serde_json::from_value(serde_json::Value::String(kind.to_owned())).ok()
}

/// The kind a published id names: `ext:pdf`, `mime:text/html`,
/// `scheme:https`.
fn parse_kind(kind: &str) -> Option<ResourceKind> {
    let (prefix, name) = kind.split_once(':')?;
    match prefix {
        "ext" => ResourceKind::extension(name).ok(),
        "mime" => ResourceKind::mime(name).ok(),
        "scheme" => ResourceKind::scheme(name).ok(),
        _ => None,
    }
    .filter(|parsed| parsed.as_str() == kind)
}

/// The widget id of the chooser of the type `file_type`: by its key, so
/// every extension of one type (`.jpeg`, `.jpg`) names the same row.
fn chooser_id(file_type: &FileType) -> String {
    format!(
        "personal-default.{}",
        file_type.key().map_or("", ResourceKind::as_str)
    )
}

/// What the area reads of the device: the installed apps that claim types
/// of files and links, and how much the person uses each category.
#[derive(Clone, Debug, Default)]
pub struct Inventory {
    pub candidates: Vec<AppCandidate>,
    pub usage: TypeUsage,
}

/// What the Default Apps area keeps for the window: the choices as last
/// read, what the device has installed and the person opened, and which
/// category is open.
pub struct DefaultAppsArea {
    prefs: RefCell<Followed<DefaultApps>>,
    inventory: OnceCell<Inventory>,
    sample: bool,
    /// The installed apps, read off the UI thread after the window's first
    /// frame ([`Self::start_read`]).
    read: crate::device_read::DeviceRead<Inventory>,
    /// The rows, kept for the defaults version and locale they show.
    rows: crate::host::RowsMemo<(ui::storage::SettingsRevision, u64)>,
    /// The open category, one at a time, kept across the pane's rebuilds.
    groups: DisclosureGroup,
    /// The type a search hit asked to show: listed even where no app
    /// claims it and the person has no file of it.
    revealed: RefCell<Option<ResourceKind>>,
}

impl DefaultAppsArea {
    /// The area over `cx`'s settings. A capture lists sample apps rather
    /// than the ones this device has installed, and sets up what its
    /// scenario shows.
    pub fn new(cx: &PaneCx) -> Self {
        let area = Self {
            prefs: RefCell::new(cx.follow::<DefaultApps>()),
            inventory: OnceCell::new(),
            sample: !cx.reaches_devices(),
            read: crate::device_read::DeviceRead::default(),
            rows: crate::host::RowsMemo::default(),
            groups: DisclosureGroup::new(),
            revealed: RefCell::new(None),
        };
        if let Some(scene) = cx.scene() {
            let scene = &scene.default_apps;
            if let Some(open) = scene.open {
                area.groups.open(open.slug());
            }
            if !scene.categories.is_empty() || !scene.changed.is_empty() {
                let _ = cx.settings::<DefaultApps>().update(|stored| {
                    for (category, app) in &scene.categories {
                        stored
                            .categories
                            .insert(category.slug().to_owned(), app.clone());
                    }
                    for (kind, app) in &scene.changed {
                        stored.handlers.insert(kind.clone(), app.clone());
                    }
                });
            }
        }
        area
    }

    fn inventory(&self) -> &Inventory {
        self.inventory.get_or_init(|| {
            self.read
                .take()
                .unwrap_or_else(|| Self::read_inventory(self.sample))
        })
    }

    fn candidates(&self) -> &[AppCandidate] {
        &self.inventory().candidates
    }

    /// Every installed app's manifest claims and the person's recent
    /// files, or the sample a capture lists.
    fn read_inventory(sample: bool) -> Inventory {
        if sample {
            sample_inventory()
        } else {
            Inventory {
                candidates: installed_candidates(),
                usage: desktop_policy::usage::recent_type_usage(),
            }
        }
    }

    /// Read the installed apps off the UI thread; `false` when they have
    /// been read or are being read.
    pub(crate) fn start_read(&self) -> bool {
        if self.is_open() {
            return false;
        }
        let sample = self.sample;
        self.read.start(move || Self::read_inventory(sample))
    }

    /// Whether the read [`Self::start_read`] started has ended.
    pub(crate) fn read_finished(&self) -> bool {
        self.read.is_finished()
    }

    /// Whether the area has read what its rows show: its pane was shown,
    /// an effect or a row lookup reached it, or the window read it after
    /// its first frame ([`crate::panes::LIVE_AREAS`]).
    pub(crate) fn is_open(&self) -> bool {
        self.inventory.get().is_some()
    }

    /// Where `anchor` (a type's row, `settings.default-app:ext:rs`) is
    /// shown: its category opened, its chooser taking the keyboard.
    fn reveal_type(&self, anchor: &str) -> Option<crate::area::Reveal> {
        let kind = parse_kind(anchor.strip_prefix(crate::effects::DEFAULT_APP_ANCHOR)?)?;
        let file_type = FileType::of(kind.clone());
        self.groups.open(file_type.category().slug());
        *self.revealed.borrow_mut() = Some(kind);
        Some(crate::area::Reveal {
            pane: crate::categories::Category::DefaultApps,
            control: chooser_id(&file_type),
        })
    }

    /// One live row per category, offering its apps for the whole
    /// category, and under it one per type, showing the app that opens it
    /// now and offering every app that claims it.
    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        let mut prefs = self.prefs.borrow_mut();
        prefs.refresh();
        let key = (prefs.revision(), ui::i18n::I18nManager::revision());
        self.rows.rows(key, || self.make_rows(prefs.get()))
    }

    fn make_rows(&self, prefs: &DefaultApps) -> Vec<LiveRow> {
        let defaults = prefs.personal();
        let inventory = self.inventory();
        model(inventory, &defaults, None)
            .iter()
            .map(category_live_row)
            .collect()
    }
}

/// One category as the page shows it.
struct CategoryModel<'a> {
    category: TypeCategory,
    /// Every app that claims something of the category, by name.
    apps: Vec<&'a AppCandidate>,
    /// The types the page lists: those an app claims, those the person
    /// opened recently and the one a search hit asked for.
    types: Vec<TypeModel<'a>>,
    /// The app the person chose for the whole category, if it still may be.
    chosen: Option<&'a AppCandidate>,
    /// What the category's types open with.
    opens: Opens<'a>,
}

/// What a category's types open with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Opens<'a> {
    /// All with this app.
    App(&'a AppCandidate),
    /// Not all with one app.
    Mixed,
    /// No app opens any of them.
    Nothing,
}

impl CategoryModel<'_> {
    /// No app claims anything of it and the person has no file of it.
    fn is_empty(&self) -> bool {
        self.apps.is_empty() && self.types.is_empty()
    }

    /// The app the category's chooser shows: the person's choice, else the
    /// one app every type opens with.
    fn shown(&self) -> Option<&AppCandidate> {
        self.chosen.or(match self.opens {
            Opens::App(app) => Some(app),
            Opens::Mixed | Opens::Nothing => None,
        })
    }

    /// The line under the category's name: "Opens with Writer", "Mixed",
    /// "No app installed".
    fn summary(&self) -> String {
        match self.opens {
            Opens::App(app) => t_args(
                "settings-default-apps-opens-with",
                &[("app", ArgValue::Text(app.display_name.clone()))],
            ),
            Opens::Mixed => t("settings-default-apps-mixed"),
            Opens::Nothing => t("settings-default-apps-no-app"),
        }
    }
}

/// One type as the page shows it.
struct TypeModel<'a> {
    file_type: FileType,
    /// The extensions the person sees for it, or the link it is.
    title: String,
    /// Every app that claims it, by name.
    apps: Vec<&'a AppCandidate>,
    /// The app it opens with now.
    current: Option<&'a AppCandidate>,
    /// The person chose its app themselves, and it differs from the
    /// category's.
    changed: bool,
}

/// The page's categories over `inventory` and the person's `defaults`:
/// those with an app or files first, the most used first, Other last among
/// them; then the empty ones.
fn model<'a>(
    inventory: &'a Inventory,
    defaults: &PersonalDefaults,
    revealed: Option<&ResourceKind>,
) -> Vec<CategoryModel<'a>> {
    let candidates = &inventory.candidates;
    let resolver = DefaultAppResolver::new(candidates);
    let find = |package: &str| candidates.iter().find(|app| app.package_id == package);
    let mut categories: Vec<CategoryModel<'a>> = TypeCategory::ALL
        .iter()
        .map(|&category| {
            let apps = by_name(
                candidates
                    .iter()
                    .filter(|app| app.resources.iter().any(|kind| kind.category() == category))
                    .collect(),
            );
            let types: Vec<TypeModel<'a>> = types_of(inventory, category, revealed)
                .into_iter()
                .take(MAX_KIND_ROWS)
                .map(|(file_type, title)| {
                    let current = resolver
                        .resolve_type(&file_type, defaults)
                        .and_then(|selected| find(&selected.package_id));
                    let inherited = resolver
                        .inherited(&file_type, defaults)
                        .and_then(|selected| find(&selected.package_id));
                    let changed = file_type.is_overridden(defaults)
                        && current.map(|app| &app.package_id)
                            != inherited.map(|app| &app.package_id);
                    let apps = by_name(
                        candidates
                            .iter()
                            .filter(|app| file_type.claimed_by(app))
                            .collect(),
                    );
                    TypeModel {
                        file_type,
                        title,
                        apps,
                        current,
                        changed,
                    }
                })
                .collect();
            let chosen = defaults
                .category_app(category)
                .filter(|package| resolver.validate_category_app(category, package).is_ok())
                .and_then(find);
            let opened: Vec<&AppCandidate> = types.iter().filter_map(|ty| ty.current).collect();
            let opens = match opened.as_slice() {
                [] => Opens::Nothing,
                [first, rest @ ..] if rest.iter().all(|app| app.package_id == first.package_id) => {
                    Opens::App(*first)
                }
                _ => Opens::Mixed,
            };
            CategoryModel {
                category,
                apps,
                types,
                chosen,
                opens,
            }
        })
        .collect();
    let place = |model: &CategoryModel<'_>| {
        TypeCategory::ALL
            .iter()
            .position(|category| *category == model.category)
            .unwrap_or(usize::MAX)
    };
    categories.sort_by_key(|model| {
        (
            model.is_empty(),
            model.category == TypeCategory::Other,
            std::cmp::Reverse(inventory.usage.count(model.category)),
            place(model),
        )
    });
    categories
}

/// The types `category` lists, with their titles: the extensions apps
/// claim, those of the person's recent files and `revealed`, one row per
/// type (`.jpeg, .jpg`), then its links.
fn types_of(
    inventory: &Inventory,
    category: TypeCategory,
    revealed: Option<&ResourceKind>,
) -> Vec<(FileType, String)> {
    let mut extensions: BTreeSet<String> = BTreeSet::new();
    let mut links: BTreeSet<ResourceKind> = BTreeSet::new();
    let claimed = inventory
        .candidates
        .iter()
        .flat_map(|app| app.resources.iter());
    for kind in claimed.chain(revealed) {
        if kind.category() != category {
            continue;
        }
        if let Some(ext) = kind.extension_name() {
            extensions.insert(ext.to_owned());
        } else if kind.scheme_name().is_some() {
            links.insert(kind.clone());
        }
    }
    if let Some(recent) = inventory.usage.extensions.get(&category) {
        extensions.extend(recent.iter().cloned());
    }
    // One row per type: the extensions that share their own MIME type.
    let mut grouped: BTreeMap<String, (FileType, Vec<String>)> = BTreeMap::new();
    for ext in extensions {
        let Ok(kind) = ResourceKind::extension(&ext) else {
            continue;
        };
        let file_type = FileType::of(kind);
        let key = file_type
            .key()
            .map_or_else(|| ext.clone(), |key| key.as_str().to_owned());
        grouped
            .entry(key)
            .or_insert_with(|| (file_type, Vec::new()))
            .1
            .push(ext);
    }
    let mut files: Vec<(FileType, String)> = grouped
        .into_values()
        .map(|(file_type, extensions)| {
            let title = extensions
                .iter()
                .map(|ext| format!(".{ext}"))
                .collect::<Vec<_>>()
                .join(", ");
            (file_type, title)
        })
        .collect();
    files.sort_by(|a, b| a.1.cmp(&b.1));
    files.extend(links.into_iter().map(|kind| {
        let title = PersonalAppsPane::resource_label(&kind);
        (FileType::of(kind), title)
    }));
    files
}

/// `apps`, by name.
fn by_name(mut apps: Vec<&AppCandidate>) -> Vec<&AppCandidate> {
    apps.sort_by(|a, b| {
        a.display_name
            .cmp(&b.display_name)
            .then_with(|| a.package_id.cmp(&b.package_id))
    });
    apps
}

/// The live row of a category: its apps offered for the whole category,
/// then its types.
fn category_live_row(model: &CategoryModel<'_>) -> LiveRow {
    let slug = model.category.slug();
    let name = type_category(model.category);
    let id = format!("settings.default-app-category:{slug}");
    let mut row = LiveRow::new(id.clone(), "settings.default-app-category", name.clone())
        .presentation(ResourcePresentation {
            detail: Some(model.summary()),
            ..ResourcePresentation::default()
        });
    for app in model.apps.iter().take(MAX_APP_ROWS) {
        let mut choose = SettingsEffect::from(DefaultAppsEffect::SetCategoryApp {
            category: slug.to_owned(),
            app: app.package_id.clone(),
        })
        .bound();
        choose.label = format!("Open all {name} with {}", app.display_name);
        row = row.row(
            LiveRow::new(
                format!("{id}/{}", app.package_id),
                "settings.default-app-candidate",
                app.display_name.clone(),
            )
            .operation(choose),
        );
    }
    for ty in &model.types {
        row = row.row(type_live_row(ty));
    }
    row
}

/// The live row of a type: the app it opens with now, a reset when the
/// person chose it, and every app that claims it.
fn type_live_row(ty: &TypeModel<'_>) -> LiveRow {
    let Some(kind) = ty.file_type.primary() else {
        return LiveRow::new(
            String::new(),
            "settings.default-app-handler",
            ty.title.clone(),
        );
    };
    let id = crate::effects::default_app_row_id(kind.as_str());
    let mut row = LiveRow::new(id.clone(), "settings.default-app-handler", ty.title.clone());
    if let Some(current) = ty.current {
        row = row.presentation(ResourcePresentation {
            detail: Some(current.package_id.clone()),
            ..ResourcePresentation::default()
        });
    }
    if ty.changed {
        let mut reset = SettingsEffect::from(DefaultAppsEffect::ResetDefaultApp {
            kind: kind.as_str().to_owned(),
        })
        .bound();
        reset.label = format!("Open {} with the category's app", ty.title);
        row = row.operation(reset);
    }
    for app in ty.apps.iter().take(MAX_APP_ROWS) {
        let mut choose = SettingsEffect::from(DefaultAppsEffect::SetDefaultApp {
            kind: kind.as_str().to_owned(),
            app: app.package_id.clone(),
        })
        .bound();
        choose.label = format!(
            "Open {} with {}",
            PersonalAppsPane::resource_label(kind),
            app.display_name
        );
        row = row.row(
            LiveRow::new(
                format!("{id}/{}", app.package_id),
                "settings.default-app-candidate",
                app.display_name.clone(),
            )
            .operation(choose),
        );
    }
    row
}

/// The apps a capture lists: the system's apps with the types their
/// manifests claim, and a code editor from the Store.
const SAMPLE_APPS: &[(&str, &str, &[&str])] = &[
    (
        "so.tilo.word-processor",
        "Word Processor",
        &[
            "ext:odt",
            "ext:docx",
            "ext:md",
            "ext:markdown",
            "ext:rtf",
            "ext:html",
            "ext:htm",
            "mime:application/vnd.oasis.opendocument.text",
            "mime:text/html",
        ],
    ),
    (
        "so.tilo.sheets",
        "Sheets",
        &["ext:ods", "ext:xlsx", "ext:csv"],
    ),
    ("so.tilo.slides", "Slides", &["ext:odp", "ext:pptx"]),
    (
        "so.tilo.pdf-viewer",
        "PDF Viewer",
        &["ext:pdf", "mime:application/pdf"],
    ),
    (
        "so.tilo.photos",
        "Photos",
        &["ext:png", "ext:jpg", "ext:jpeg", "ext:webp"],
    ),
    (
        "so.tilo.canvas",
        "Canvas",
        &["ext:artwork", "ext:png", "ext:jpg", "ext:jpeg", "ext:webp"],
    ),
    (
        "so.tilo.video-player",
        "Video Player",
        &["ext:mp4", "ext:mkv", "ext:webm"],
    ),
    (
        "so.tilo.video-editor",
        "Video Editor",
        &[
            "ext:mp4", "ext:mov", "ext:mkv", "ext:webm", "ext:mp3", "ext:wav",
        ],
    ),
    (
        "so.tilo.music",
        "Music",
        &[
            "ext:mp3", "ext:flac", "ext:wav", "ext:ogg", "ext:opus", "ext:m4a",
        ],
    ),
    (
        "so.tilo.code-editor",
        "Code Editor",
        &[
            "ext:rs", "ext:toml", "ext:py", "ext:js", "ext:ts", "ext:css", "ext:yaml", "ext:yml",
            "ext:sh", "ext:c", "ext:h", "ext:cpp", "ext:go",
        ],
    ),
    (
        "dev.zed.Zed",
        "Zed",
        &[
            "ext:rs", "ext:toml", "ext:py", "ext:js", "ext:ts", "ext:json", "ext:yaml", "ext:yml",
            "ext:go",
        ],
    ),
    ("so.tilo.chip-design", "Chip Design", &["ext:json"]),
    ("so.tilo.notes", "Notes", &["ext:md", "ext:markdown"]),
    ("so.tilo.typst-editor", "Typst Editor", &["ext:typ"]),
    (
        "so.tilo.browser",
        "Browser",
        &["ext:html", "ext:htm", "scheme:http", "scheme:https"],
    ),
    (
        "so.tilo.feeds",
        "Feeds",
        &["ext:rss", "ext:atom", "ext:opml"],
    ),
    ("so.tilo.mail", "Mail", &["ext:eml", "scheme:mailto"]),
    (
        "so.tilo.calendar",
        "Calendar",
        &["ext:ics", "scheme:webcal"],
    ),
    ("so.tilo.contacts", "Contacts", &["ext:vcf"]),
    ("so.tilo.ebook", "E-Book Reader", &["ext:epub"]),
    (
        "so.tilo.store",
        "App Store",
        &["ext:flatpakref", "ext:tapp", "scheme:tilo-store"],
    ),
    ("so.tilo.maps", "Maps", &["scheme:geo"]),
];

/// The apps a capture lists picked for what they claim on purpose, rather
/// than opening it by default.
const SAMPLE_SECONDARY: &[&str] = &["so.tilo.video-editor"];

/// The files a capture's person opened recently, by extension: mostly
/// code, then documents and photos.
const SAMPLE_RECENT: &[(&str, u32)] = &[
    ("rs", 24),
    ("toml", 9),
    ("py", 5),
    ("json", 4),
    ("log", 3),
    ("pdf", 6),
    ("md", 7),
    ("odt", 3),
    ("docx", 2),
    ("jpg", 8),
    ("png", 6),
    ("xlsx", 3),
    ("csv", 2),
    ("mp3", 3),
    ("mp4", 2),
    ("html", 2),
    ("zip", 2),
    ("eml", 1),
    ("pptx", 1),
];

/// The apps and recent files a capture shows.
fn sample_inventory() -> Inventory {
    let candidates = SAMPLE_APPS
        .iter()
        .map(|(package, name, kinds)| AppCandidate {
            package_id: (*package).into(),
            display_name: (*name).into(),
            resources: kinds.iter().filter_map(|kind| parse_kind(kind)).collect(),
            secondary: SAMPLE_SECONDARY.contains(package),
        })
        .collect();
    let mut usage = TypeUsage::default();
    for (ext, times) in SAMPLE_RECENT {
        for _ in 0..*times {
            usage.note(ext);
        }
    }
    Inventory { candidates, usage }
}

pub struct PersonalAppsPane {
    cx: PaneCx,
    inventory: Inventory,
    prefs: Followed<DefaultApps>,
    groups: DisclosureGroup,
    revealed: Option<ResourceKind>,
    composed: ui::widget::Composed,
}

impl PersonalAppsPane {
    /// The pane over `cx`'s choices, among `area`'s apps.
    pub fn new(cx: PaneCx, area: &DefaultAppsArea) -> Self {
        let inventory = area.inventory().clone();
        let prefs = cx.follow::<DefaultApps>();
        Self {
            cx,
            inventory,
            prefs,
            groups: area.groups.clone(),
            revealed: area.revealed.borrow().clone(),
            composed: ui::widget::Composed::default(),
        }
    }

    fn resource_label(resource: &ResourceKind) -> String {
        if let Some(ext) = resource.extension_name() {
            t_args(
                "settings-personal-apps-ext-files",
                &[("ext", ArgValue::Text(ext.to_string()))],
            )
        } else if let Some(scheme) = resource.scheme_name() {
            t_args(
                "settings-personal-apps-scheme-links",
                &[("scheme", ArgValue::Text(scheme.to_string()))],
            )
        } else {
            resource.display_label()
        }
    }

    /// A chooser among `apps`, showing `current`, or `placeholder` when no
    /// one app is current.
    fn chooser(
        id: &str,
        apps: &[&AppCandidate],
        current: Option<&AppCandidate>,
        placeholder: &str,
    ) -> Dropdown {
        let options: Vec<String> = apps.iter().map(|app| app.package_id.clone()).collect();
        let named: Vec<(String, String)> = apps
            .iter()
            .map(|app| (app.package_id.clone(), app.display_name.clone()))
            .collect();
        let state = LocalState::new(
            current
                .map(|app| app.package_id.clone())
                .unwrap_or_default(),
        );
        let shown = current.map_or(placeholder, |app| app.display_name.as_str());
        let mut chooser = Dropdown::from_state(options, &state)
            .with_id(id)
            .with_placeholder(shown);
        chooser.set_value_options(named);
        chooser
    }

    /// The row of one category: its icon, its name and what its types
    /// open with; open, its app for every type and then each type.
    fn category_row(&self, model: &CategoryModel<'_>) -> Box<dyn Widget> {
        let slug = model.category.slug();
        let name = type_category(model.category);
        let icon = Icon::from_name(model.category.icon()).unwrap_or(Icon::File);
        if model.is_empty() {
            // Nothing to choose: it stays closed, at the bottom.
            return Box::new(
                ListRow::new(format!("personal-default-category.{slug}"), name)
                    .icon(icon)
                    .secondary(t("settings-default-apps-no-app")),
            );
        }
        let mut items: Vec<Box<dyn Widget>> = Vec::new();
        if !model.apps.is_empty() {
            let placeholder = t("settings-default-apps-mixed");
            let chooser = Self::chooser(
                &format!("personal-default-category.{slug}"),
                &model.apps,
                model.shown(),
                &placeholder,
            )
            .with_accessible_name(t_args(
                "settings-default-apps-category-chooser",
                &[("category", ArgValue::Text(name.clone()))],
            ));
            let category = slug.to_owned();
            let chooser = self.cx.bind(
                chooser,
                effect(DefaultAppsEffect::SET_CATEGORY_APP)
                    .map(move |package: String| (category.clone(), package)),
            );
            items.push(Box::new(
                ListRow::new(
                    format!("personal-default-category-row.{slug}"),
                    t("settings-default-apps-all-types"),
                )
                .trailing(compact_control(Box::new(chooser))),
            ));
        }
        for ty in &model.types {
            items.push(self.type_row(ty));
        }
        Box::new(
            DisclosureRow::keyed(name, Box::new(Section::new(items)), &self.groups, slug)
                .icon(icon)
                .secondary(model.summary()),
        )
    }

    /// The row of one type: its extensions, the app it opens with, and a
    /// quiet "Changed" with a reset when the person chose that app
    /// themselves.
    fn type_row(&self, ty: &TypeModel<'_>) -> Box<dyn Widget> {
        let id = chooser_id(&ty.file_type);
        let Some(kind) = ty.file_type.primary() else {
            return Box::new(ListRow::new(id, ty.title.clone()));
        };
        if ty.apps.is_empty() {
            return Box::new(
                ListRow::new(id, ty.title.clone()).value(t("settings-default-apps-no-app")),
            );
        }
        let named = kind.as_str().to_owned();
        let chooser =
            Self::chooser(&id, &ty.apps, ty.current, &ty.title).with_accessible_name(t_args(
                "settings-default-apps-type-chooser",
                &[("type", ArgValue::Text(ty.title.clone()))],
            ));
        let chooser = compact_control(Box::new(
            self.cx.bind(
                chooser,
                effect(DefaultAppsEffect::SET_DEFAULT_APP)
                    .map(move |package: String| (named.clone(), package)),
            ),
        ));
        let row = ListRow::new(
            format!("personal-default-row.{}", kind.as_str()),
            ty.title.clone(),
        );
        if !ty.changed {
            return Box::new(row.trailing(chooser));
        }
        let reset = self.cx.bind_invocation(
            Button::new(
                &format!("personal-default-reset.{}", kind.as_str()),
                &t("settings-default-apps-reset"),
            )
            .role(ButtonRole::Plain)
            .control_size(ControlSize::Small)
            .with_accessible_name(&t_args(
                "settings-default-apps-reset-type",
                &[("type", ArgValue::Text(ty.title.clone()))],
            )),
            DefaultAppsEffect::ResetDefaultApp {
                kind: kind.as_str().to_owned(),
            },
        );
        Box::new(
            row.secondary(t("settings-default-apps-changed"))
                .trailing(Box::new(
                    Row::new()
                        .no_wrap()
                        .with_align_items(ui::taffy::AlignItems::Center)
                        .with_rhythm(ui::tokens::Rhythm::Related)
                        .push(Box::new(reset))
                        .push(chooser),
                )),
        )
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let defaults = self.prefs.get().personal();
        let categories = model(&self.inventory, &defaults, self.revealed.as_ref());
        let mut items: Vec<_> = categories
            .iter()
            .map(|model| row(self.category_row(model)))
            .collect();
        if self.inventory.candidates.is_empty() {
            items.push(note(&t("settings-personal-apps-empty")));
        } else {
            items.push(note(&t("settings-personal-apps-choice-follows")));
        }
        // A workspace's own rules are that workspace's: one row opens them.
        let workspace = untitled(rows(vec![setting_row_described(
            &t("settings-personal-apps-workspace-rules"),
            &t("settings-personal-apps-scope-caption"),
            Box::new(self.cx.bind_invocation(
                Button::new(
                    "open-workspace-app-defaults",
                    &t("settings-personal-apps-open"),
                ),
                DefaultAppsEffect::OpenWorkspaceDefaults,
            )),
        )]));
        crate::widgets::page(vec![
            section(&t("settings-personal-apps-files-title"), rows(items)),
            workspace,
        ])
    }
}

/// Ask the shell to open Workspace Settings at its app defaults: a round
/// trip to the shell, so it runs off the UI thread.
fn open_workspace_defaults() -> Result<(), String> {
    crate::device_read::off_ui_thread();
    os_actions::open_tool(os_actions::ToolIntent::WorkspaceSettings {
        route: os_actions::WorkspaceSettingsRoute::AppsFiles,
    })
}

/// Read real installed manifests. A type only appears if an installed GUI app
/// claims it; Settings does not invent handlers.
pub fn installed_candidates() -> Vec<AppCandidate> {
    let mut out = Vec::new();
    for entry in package_store::list_installed().unwrap_or_default() {
        let Ok(manifest) = tilo_app_package::view::read_manifest(&entry.manifest_path) else {
            continue;
        };
        if manifest.entrypoint.gui_bin().is_none() {
            continue;
        }
        let resources = manifest
            .opens_ext
            .iter()
            .filter_map(|e| ResourceKind::extension(e).ok())
            .chain(
                manifest
                    .opens_mime
                    .iter()
                    .filter_map(|mime| ResourceKind::mime(mime).ok()),
            )
            .chain(
                manifest
                    .opens_schemes
                    .iter()
                    .filter_map(|scheme| ResourceKind::scheme(scheme).ok()),
            )
            .collect();
        let package_id = if manifest.package_id.is_empty() {
            entry.id
        } else {
            manifest.package_id
        };
        out.push(AppCandidate {
            display_name: manifest
                .name
                .unwrap_or_else(|| package_store::display_name_for_package_id(&package_id)),
            package_id,
            resources,
            secondary: manifest.opens_secondary,
        });
    }
    out.sort_by(|a, b| {
        a.display_name
            .cmp(&b.display_name)
            .then_with(|| a.package_id.cmp(&b.package_id))
    });
    out
}

impl Widget for PersonalAppsPane {
    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        // A relayout keeps the rows; a changed choice rebuilds them, as
        // each names the app it shows.
        if self.prefs.refresh() {
            self.composed.invalidate();
        }
        self.composed.sync(state, root);
    }

    crate::panes::composed_pane!(kept);

    fn name(&self) -> &'static str {
        "PersonalAppsPane"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::SettingsShell;
    use ui::prelude::TestApp;

    fn candidate(id: &str, ext: &str) -> AppCandidate {
        AppCandidate {
            package_id: id.into(),
            display_name: id.into(),
            resources: [ResourceKind::extension(ext).unwrap()].into(),
            secondary: false,
        }
    }

    fn focused(widget: &dyn Widget) -> bool {
        widget.is_focused() || widget.children().into_iter().any(focused)
    }

    fn ext(ext: &str) -> ResourceKind {
        ResourceKind::extension(ext).unwrap()
    }

    /// What the page shows for the type `kind`: the app it opens with, and
    /// whether it is marked changed.
    fn shown(app: &mut TestApp<SettingsShell>, kind: &str) -> (Option<String>, bool) {
        let defaults = app.settings::<DefaultApps>().get().personal();
        let inventory = sample_inventory();
        let wanted = FileType::of(parse_kind(kind).unwrap());
        model(&inventory, &defaults, None)
            .iter()
            .flat_map(|category| category.types.iter())
            .find(|ty| ty.file_type.key() == wanted.key())
            .map(|ty| (ty.current.map(|app| app.package_id.clone()), ty.changed))
            .unwrap_or_else(|| panic!("the page lists {kind}"))
    }

    #[test]
    fn personal_apps_copy_comes_from_the_catalogue() {
        assert_eq!(
            t("settings-personal-apps-files-title"),
            "Open files and links with"
        );
        assert_eq!(
            PersonalAppsPane::resource_label(&ResourceKind::extension("pdf").unwrap()),
            "PDF documents"
        );
        assert_eq!(
            PersonalAppsPane::resource_label(&ResourceKind::scheme("https").unwrap()),
            "Secure web links"
        );
    }

    /// A choice is the person's `default_apps` section, which the launcher
    /// reads; an app that does not claim the kind is refused, and showing
    /// the resolver's pick stores nothing.
    #[test]
    fn a_default_app_is_the_persons_choice() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let settings = app.settings::<DefaultApps>();
        assert!(
            settings.get().handlers.is_empty(),
            "showing the resolver's pick is not a personal choice"
        );
        app.effect_on(
            "settings.default-app:ext:pdf/so.tilo.pdf-viewer",
            &SettingsEffect::from(DefaultAppsEffect::SetDefaultApp {
                kind: "ext:pdf".into(),
                app: "so.tilo.pdf-viewer".into(),
            }),
        )
        .expect("the app's row offers making it the default");
        let section = settings
            .document()
            .section(desktop_policy::PERSONAL_DEFAULTS_SECTION);
        assert_eq!(
            PersonalDefaults::from_section(&section)
                .handlers
                .get(&ResourceKind::extension("pdf").unwrap())
                .map(String::as_str),
            Some("so.tilo.pdf-viewer")
        );
        let refused = DefaultAppsEffect::SetDefaultApp {
            kind: "ext:pdf".into(),
            app: "so.tilo.photos".into(),
        };
        assert!(app.effect(&SettingsEffect::from(refused)).is_err());
        let surface = app.agent_surface().to_string();
        assert!(
            surface.contains("settings.default-app:scheme:https"),
            "{surface}"
        );
        assert!(
            surface.contains("settings.default-app-category:code"),
            "{surface}"
        );
    }

    #[test]
    fn choosing_a_default_preserves_other_saved_handlers() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let settings = app.settings::<DefaultApps>();
        // Open the area's cached snapshot before another writer changes it.
        let _ = app.agent_surface();
        settings
            .update(|stored| {
                stored
                    .handlers
                    .insert("ext:txt".into(), "so.tilo.notes".into());
                // A newer reader may understand kinds this version cannot parse.
                stored
                    .handlers
                    .insert("future:document".into(), "so.tilo.future".into());
                stored
                    .categories
                    .insert("holograms".into(), "so.tilo.future".into());
            })
            .unwrap();
        app.effect(&SettingsEffect::from(DefaultAppsEffect::SetDefaultApp {
            kind: "ext:pdf".into(),
            app: "so.tilo.pdf-viewer".into(),
        }))
        .expect("choose the PDF handler");
        app.effect(&SettingsEffect::from(DefaultAppsEffect::SetCategoryApp {
            category: "code".into(),
            app: "dev.zed.Zed".into(),
        }))
        .expect("choose the code editor");
        let stored = settings.get();
        let handlers = &stored.handlers;
        assert_eq!(
            handlers.get("ext:pdf").map(String::as_str),
            Some("so.tilo.pdf-viewer")
        );
        assert_eq!(
            handlers.get("ext:txt").map(String::as_str),
            Some("so.tilo.notes")
        );
        assert_eq!(
            handlers.get("future:document").map(String::as_str),
            Some("so.tilo.future")
        );
        assert_eq!(
            stored.categories.get("holograms").map(String::as_str),
            Some("so.tilo.future")
        );
        assert_eq!(
            stored.categories.get("code").map(String::as_str),
            Some("dev.zed.Zed")
        );
    }

    /// The category's app reaches every type without one of its own; a
    /// type's own app survives the category's change and reads "Changed"
    /// until reset.
    #[test]
    fn a_category_choice_reaches_its_types_and_a_types_own_choice_stays() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let _ = app.agent_surface();
        assert_eq!(
            shown(&mut app, "ext:toml"),
            (Some("so.tilo.code-editor".into()), false)
        );
        app.effect(&SettingsEffect::from(DefaultAppsEffect::SetDefaultApp {
            kind: "ext:rs".into(),
            app: "so.tilo.code-editor".into(),
        }))
        .expect("choose Code Editor for Rust");
        app.effect(&SettingsEffect::from(DefaultAppsEffect::SetCategoryApp {
            category: "code".into(),
            app: "dev.zed.Zed".into(),
        }))
        .expect("choose Zed for code");
        assert_eq!(
            shown(&mut app, "ext:toml"),
            (Some("dev.zed.Zed".into()), false)
        );
        assert_eq!(
            shown(&mut app, "ext:json"),
            (Some("dev.zed.Zed".into()), false)
        );
        assert_eq!(
            shown(&mut app, "ext:rs"),
            (Some("so.tilo.code-editor".into()), true),
            "the type's own choice survives and reads Changed"
        );
        assert!(
            app.effect(&SettingsEffect::from(DefaultAppsEffect::SetCategoryApp {
                category: "code".into(),
                app: "so.tilo.photos".into(),
            }))
            .is_err(),
            "an app that opens nothing of the category is refused"
        );
        app.effect(&SettingsEffect::from(DefaultAppsEffect::ResetDefaultApp {
            kind: "ext:rs".into(),
        }))
        .expect("reset Rust");
        assert_eq!(
            shown(&mut app, "ext:rs"),
            (Some("dev.zed.Zed".into()), false)
        );
    }

    /// The most used categories come first; Other is last of those with
    /// apps; a category with no app and no files is at the bottom, closed.
    #[test]
    fn categories_are_ordered_by_use_and_empty_ones_stay_closed_at_the_bottom() {
        let inventory = sample_inventory();
        let categories = model(&inventory, &PersonalDefaults::default(), None);
        let order: Vec<TypeCategory> = categories.iter().map(|c| c.category).collect();
        assert_eq!(order.first(), Some(&TypeCategory::Code), "{order:?}");
        assert_eq!(order.get(1), Some(&TypeCategory::Documents), "{order:?}");
        assert_eq!(order.get(2), Some(&TypeCategory::Images), "{order:?}");
        assert_eq!(order.len(), TypeCategory::ALL.len());
        let empty: Vec<TypeCategory> = categories
            .iter()
            .filter(|c| c.is_empty())
            .map(|c| c.category)
            .collect();
        assert_eq!(empty, [TypeCategory::Fonts]);
        assert_eq!(order.last(), Some(&TypeCategory::Fonts));
        assert_eq!(
            order.get(order.len() - 2),
            Some(&TypeCategory::Other),
            "Other is last of the categories with apps: {order:?}"
        );
        // Every type is listed once, in the one category it belongs to.
        let code = &categories[0];
        let titles: Vec<&str> = code.types.iter().map(|ty| ty.title.as_str()).collect();
        assert!(titles.contains(&".rs"), "{titles:?}");
        assert!(titles.contains(&".yaml, .yml"), "{titles:?}");
        assert!(
            titles.contains(&".log"),
            "a recent type no app claims: {titles:?}"
        );
        assert_eq!(code.opens, Opens::Mixed, "JSON opens with another app");
    }

    /// The pane shows the resolver's pick until the person chooses, then
    /// follows the choice whoever made it.
    #[test]
    fn the_pane_follows_the_choices() {
        let inventory = Inventory {
            candidates: vec![candidate("so.tilo.a", "pdf"), candidate("so.tilo.b", "pdf")],
            usage: TypeUsage::default(),
        };
        let current = |defaults: &PersonalDefaults| {
            model(&inventory, defaults, None)
                .iter()
                .find(|c| c.category == TypeCategory::Documents)
                .and_then(|c| c.types.first())
                .and_then(|ty| ty.current)
                .map(|app| app.package_id.clone())
        };
        let mut defaults = PersonalDefaults::default();
        assert_eq!(current(&defaults).as_deref(), Some("so.tilo.a"));
        defaults.handlers.insert(ext("pdf"), "so.tilo.b".into());
        assert_eq!(current(&defaults).as_deref(), Some("so.tilo.b"));
    }

    /// "open .rs with" in the platform search lands on the `.rs` row: the
    /// page opens with Code open and the keyboard on that type's chooser.
    #[test]
    fn a_search_hit_for_a_type_lands_on_its_row() {
        ui::i18n::I18nManager::init("en-US");
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        app.harness().resize((1200, 900)).unwrap();
        let _ = app.harness().poll_frame();
        app.call(
            "open",
            serde_json::json!({ "anchor": crate::effects::default_app_row_id("ext:rs") }),
        )
        .expect("the hit opens");
        for _ in 0..20 {
            app.settle();
            let _ = app.harness().poll_frame();
        }
        assert_eq!(
            app.with_widget::<SettingsApp, _>(SettingsApp::selected),
            Some(crate::categories::Category::DefaultApps)
        );
        assert_eq!(
            app.with_widget::<SettingsApp, _>(
                |settings| area(settings).and_then(|area| area.groups.open_key())
            ),
            Some(Some("code".to_owned())),
            "the type's category is open"
        );
        let id = chooser_id(&FileType::of(ext("rs")));
        let chooser = ui::widget::find_test_id(app.harness().root_widget(), &id)
            .unwrap_or_else(|| panic!("the page shows {id}"));
        assert!(focused(chooser), "the keyboard is on the type's chooser");
    }
}
