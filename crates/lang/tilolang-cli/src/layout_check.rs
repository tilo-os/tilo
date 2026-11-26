//! `tilo-layout-check` (docs/designs/app-language/layout.md, "Checking"): every view of an
//! app, staged from each of its scenarios, laid out headlessly through the app window's path at
//! the sizes, display scales and text scales people use, and what is wrong with each layout
//! reported at the view's node in its source.
//!
//! Each scenario's window is one instance and one frame pipeline, laid out again for every
//! configuration; nothing is painted or rasterised. Every scenario is a window of its own, so
//! the windows are checked at the same time, one per core.

mod audit;
pub mod command;
mod layouts;
mod reach;
mod source;
pub(crate) mod stage;
mod ux;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub use audit::Kind;
pub use layouts::{DISPLAY_SCALES, Layout, SIZES, Size, TEXT_SCALES, layouts};
pub use stage::Work;
use tilo_render_protocol::patch::ResourceRef;

use reach::{
    back_chip, destinations, reachable, sidebar_toggle, surface_presses, tappable_row, unchosen_row,
};

#[derive(Clone, Debug, serde::Serialize)]
pub struct Finding {
    pub kind: Kind,
    /// The app's short name (`notes` for `so.tilo.notes`).
    pub app: String,
    /// The view whose node it is, or `(window)` for the window's own parts.
    pub view: String,
    pub node: Option<String>,
    pub widget: String,
    pub file: Option<String>,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub detail: String,
    pub scenario: String,
    pub layouts: Vec<String>,
    pub count: usize,
    /// What an allow-list names it by ([`Finding::id`]).
    pub id: String,
    pub allowed: bool,
}

impl Finding {
    /// Where it is: `app › view › node`.
    #[must_use]
    pub fn path(&self) -> String {
        match &self.node {
            Some(node) => format!("{} › {} › {node}", self.app, self.view),
            None => format!("{} › {}", self.app, self.view),
        }
    }

    /// What an allow-list names it by: its kind, path and widget, without the line, so editing the
    /// file above it keeps it allowed.
    #[must_use]
    pub fn id(&self) -> String {
        format!("{} {} [{}]", self.kind.name(), self.path(), self.widget)
    }
}

/// What `--dump` prints at a window size: every frame the check reads, or only the frames a
/// finding comes from. A page of every frame of every view is too much to read and too much to
/// keep, so a check run over all the apps asks for the frames with findings and a run narrowed to
/// one view asks for all of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dump {
    /// `--dump WxH`: every frame the check reads.
    Every((u32, u32)),
    /// `--dump-findings WxH`: the frames a finding was read from.
    Findings((u32, u32)),
}

impl Dump {
    /// The window size it reads the frames at.
    #[must_use]
    pub fn size(self) -> (u32, u32) {
        match self {
            Self::Every(size) | Self::Findings(size) => size,
        }
    }

    /// Whether the frame at `viewport` is printed: a frame a finding came from, or any frame when
    /// every one was asked for.
    #[must_use]
    pub fn prints(self, viewport: (u32, u32), found: bool) -> bool {
        self.size() == viewport && (found || matches!(self, Self::Every(_)))
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    pub dirs: Vec<PathBuf>,
    pub search: Vec<PathBuf>,
    pub allow: Option<PathBuf>,
    /// Print the toolkit's inspection of the frames the check reads at this window size to
    /// stderr: each widget's box, clip and semantics, as the check reads them.
    pub dump: Option<Dump>,
    /// Write the window at the `dump` size, at display and text scale 1, as each stage leaves it,
    /// in the light and the dark theme, to PNGs in this directory.
    pub shots: Option<PathBuf>,
    /// Take the shots as a maximized window stands at rest: its bar hidden over the window's top
    /// edge until the person reaches for it, the app's frame filling the window.
    pub bar_at_rest: bool,
    /// Take the shots in a window this tall at the `dump` size's width, so the lower part of a
    /// long page shows; the check still reads the window at its own size.
    pub shot_height: Option<u32>,
    /// The themes `shots` are taken in: a shipped theme by name
    /// (`glass`, `block`), in its light and dark variant, or a theme
    /// bundle's directory (`tilo_ui_base::theme_bundle`); none takes the stock
    /// light and dark.
    pub themes: Vec<String>,
    /// The layer of the package to lay out, when it has more than one root (`Shell`, the `shell`
    /// record): a shot of the shell is `--root Shell`.
    pub root: Option<String>,
    /// View states every window is staged at, as the app's own code names them (`place=Task.today`, a
    /// filter of the type the state queries; `side=Side.right`, a case of its enum): the place a
    /// window stands on before anyone picks, for a shot of a state that is not the declaration's
    /// default.
    pub states: Vec<String>,
    /// Controls pressed by the words they carry, in order, before each window is read: what a
    /// person opens by pressing it (`--press "Since you looked"`, or the row of a task, which opens
    /// where it stands, or the box beside that row's title, `Mark Buy oat milk done`), where a
    /// click is the only way to reach it.
    pub presses: Vec<String>,
    /// Key presses sent to each window before it is read, in order, as
    /// [`ui::headless::Driver::key`] names them (`Tab`, `Down`, `Space`, or a chord that
    /// holds its modifiers for the press, `Shift+F10`): the layout a person reaches by keyboard,
    /// for its shot and its semantics.
    pub keys: Vec<String>,
    /// Where the pointer rests once the keys are sent, `X,Y` in logical pixels of the window:
    /// what pointing there shows (a chart's readout, a row's hover), for its shot.
    pub hovers: Vec<String>,
    /// The language the person the windows are laid out for reads and types
    /// in, as a POSIX or BCP 47 tag (`en-US`, `de_DE.UTF-8`): what every
    /// window's language is chosen against, whatever locale the machine runs
    /// in.
    pub locale: String,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            dirs: Vec::new(),
            search: Vec::new(),
            allow: None,
            dump: None,
            shots: None,
            bar_at_rest: false,
            shot_height: None,
            themes: Vec::new(),
            root: None,
            states: Vec::new(),
            presses: Vec::new(),
            keys: Vec::new(),
            hovers: Vec::new(),
            locale: DEFAULT_LOCALE.to_owned(),
        }
    }
}

/// The language the check lays its windows out for unless `--locale` names another: the source
/// language of the toolkit and of the apps it ships, so a check says the same thing on every
/// machine, whatever locale the person running it has set.
pub const DEFAULT_LOCALE: &str = "en-US";

/// The light and the dark variant of the shipped theme `name`: `default` the stock pair, another
/// name its preset and that preset's light sibling (`glass`, `glass light`).
fn theme_pair(name: &str) -> Result<[(&'static str, tilo_ui_base::Theme); 2], String> {
    if tilo_ui_base::theme::Preset::find(name) == Some(tilo_ui_base::theme::Preset::Dark)
        || name == "default"
    {
        return Ok([
            ("light", tilo_ui_base::Theme::light()),
            ("dark", tilo_ui_base::Theme::dark()),
        ]);
    }
    if tilo_ui_base::Theme::named(name).is_none() && Path::new(name).is_dir() {
        let dir = Path::new(name);
        let theme = load_theme_bundle(dir).map_err(|e| format!("--theme {name}: {e}"))?;
        return Ok([("light", theme.clone()), ("dark", theme)]);
    }
    let dark =
        tilo_ui_base::Theme::named(name).ok_or_else(|| format!("--theme {name}: no such theme"))?;
    let light =
        tilo_ui_base::Theme::named(&format!("{name} light")).unwrap_or_else(|| dark.clone());
    Ok([("light", light), ("dark", dark)])
}

/// The theme of the bundle directory `dir`: its [`tilo_ui_base::theme_bundle::DOCUMENT`] and a `<set>.wasm`
/// beside it for each set the document names, each drawn on the machine code the device's daemon
/// compiled for it when this computer has one, read from the store it keeps them in.
fn load_theme_bundle(
    dir: &Path,
) -> Result<tilo_ui_base::Theme, tilo_ui_base::theme_bundle::BundleError> {
    let compiled = runtime_portal::process_link::compiled_modules();
    tilo_ui_base::theme_bundle::load_from(dir, compiled.as_deref())
}

/// The part of a shot's file name that says which theme family it is in: the name, or a bundle
/// directory's last component.
fn family_slug(family: &str) -> String {
    Path::new(family)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(family)
        .to_owned()
}

/// A view no layout showed a node of: the app and view
/// (`app › view`), and whether any view of any app called
/// it.
#[derive(Debug, serde::Serialize)]
pub struct Unreached {
    /// `app › view`.
    pub view: String,
    /// Whether a view calls it, its own package's or one that uses it: a view nothing calls is one
    /// no window can show, and one something calls is a screen no scenario reached.
    pub called: bool,
}

impl std::fmt::Display for Unreached {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.view)
    }
}

#[derive(Debug, serde::Serialize)]
pub struct Report {
    pub findings: Vec<Finding>,
    pub stale: Vec<String>,
    /// Views no layout showed a node of (`app › view`), a list's rows included: either nothing
    /// calls them or no scenario reaches them.
    pub unreached: Vec<Unreached>,
    pub layouts: usize,
    /// What laying every window out cost, in the toolkit's own units rather than in time: the
    /// frames laid out, the layout nodes built and the text shaped outside the cache.
    pub work: Work,
    pub elapsed_ms: u64,
}

impl Report {
    pub fn new_findings(&self) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(|f| !f.allowed)
    }
}

/// The ids an allow-list file names: one per line, `#` starting a comment.
pub fn read_allow(path: &Path) -> Result<BTreeSet<String>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(text
        .lines()
        .map(|line| line.split_once('#').map_or(line, |(kept, _)| kept).trim())
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Check every app of `options`.
pub fn check(options: &Options) -> Result<Report, String> {
    let started = std::time::Instant::now();
    // Every worker below takes a core, so a second check at the same time only slows both: the
    // machine is held for the whole check, the views it opens under the same lock.
    let _machine = machine_lock()?;
    let (apps, allow) = open(options)?;
    let scenes = scenes_of(&apps);
    let mut planned = Vec::with_capacity(scenes.len().saturating_mul(3));
    for scene in &scenes {
        planned.extend(scene.units());
    }
    let mut phase = run(&apps, &planned, options)?;
    // The views nothing showed yet, opened on the scenario whose resources they take, with what
    // every scene before it showed: the scenes of one app decide in order, as the check ran them
    // one after another, while the apps decide at the same time.
    let (mut opened, shown) = run_opens(&apps, &scenes, &phase, options)?;
    // A dump is read as a page of one layout after another: the units ran at the same time, and
    // their pages are printed where the work stands, in the order the check would have done it
    // alone.
    for found in phase.iter().chain(opened.iter()) {
        eprint!("{}", found.dump);
    }
    let (layouts, work, gathered) = gather(&scenes, &mut phase, &mut opened);
    refuse_a_missing_press(&phase, &opened, options)?;
    let mut findings: Vec<Finding> = gathered
        .into_iter()
        .flat_map(BTreeMap::into_values)
        .collect();
    let unreached = unreached_views(&apps, &shown);
    let mut seen = BTreeSet::new();
    for finding in &mut findings {
        finding.id = finding.id();
        finding.allowed = allow.contains(&finding.id);
        seen.insert(finding.id.clone());
    }
    let stale = allow.difference(&seen).cloned().collect();
    Ok(Report {
        findings,
        stale,
        unreached,
        layouts,
        work,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

/// Every app `options` names, and the findings the allow-list holds.
fn open(options: &Options) -> Result<(Vec<App>, BTreeSet<String>), String> {
    // Without the product's catalog this process's text falls back to the host's faces, so every
    // judgement about how it lays out would be about a typeface the product does not ship.
    if let Some(problem) = tilo_ui_base::text_render::font_catalog_problem() {
        return Err(problem);
    }
    tilo_ui_base::clock::freeze_for_capture();
    stage_person_locale(&options.locale)?;
    tilo_app_window::i18n::ensure();
    let allow = options
        .allow
        .as_deref()
        .map(read_allow)
        .transpose()?
        .unwrap_or_default();
    let apps: Vec<App> = options
        .dirs
        .iter()
        .map(|dir| App::read(dir, &options.search, options.root.as_deref()))
        .collect::<Result<_, String>>()?;
    Ok((apps, allow))
}

/// Every scenario of every app, on every layer it has: the units of work, each a window and a frame
/// pipeline of its own, so they run at the same time.
fn scenes_of(apps: &[App]) -> Vec<Scene> {
    apps.iter()
        .enumerate()
        .flat_map(|(app, held)| {
            held.roots.iter().flat_map(move |root| {
                held.meta.scenarios.iter().map(move |scenario| Scene {
                    app,
                    root: root.clone(),
                    scenario: scenario.name.clone(),
                })
            })
        })
        .collect()
}

/// What every scene's units found: the layouts they ran,
/// the work they did, and each app's findings, one per
/// node.
fn gather(
    scenes: &[Scene],
    phase: &mut [Found],
    opened: &mut [Found],
) -> (usize, Work, Vec<BTreeMap<Identity, Finding>>) {
    let mut layouts: usize = 0;
    let mut work = Work::default();
    let mut gathered: Vec<BTreeMap<Identity, Finding>> = Vec::new();
    gathered.resize_with(scenes.len(), BTreeMap::new);
    for ((scene, units), opened) in scenes
        .iter()
        .zip(phase.chunks_mut(3))
        .zip(opened.iter_mut())
    {
        let [scenario, press, push] = units else {
            continue;
        };
        layouts = layouts
            .saturating_add(scenario.layouts)
            .saturating_add(press.layouts)
            .saturating_add(push.layouts)
            .saturating_add(opened.layouts);
        for found in [&*scenario, &*press, &*push, &*opened] {
            work.add(found.work);
        }
        let Some(held) = gathered.get_mut(scene.app) else {
            continue;
        };
        merge(held, std::mem::take(&mut scenario.gathered));
        merge(held, std::mem::take(&mut opened.gathered));
        merge(held, std::mem::take(&mut press.gathered));
        merge(held, std::mem::take(&mut push.gathered));
    }
    (layouts, work, gathered)
}

/// Refuse a `--press` label no window carried anywhere: a control a scenario shows and the next
/// does not — a window's change mark — is pressed where it stands, rather than leaving every
/// scenario without it unpressed.
fn refuse_a_missing_press(
    phase: &[Found],
    opened: &[Found],
    options: &Options,
) -> Result<(), String> {
    let mut pressed = BTreeSet::new();
    for found in phase.iter().chain(opened.iter()) {
        pressed.extend(found.pressed.iter().cloned());
    }
    if let Some(missing) = options
        .presses
        .iter()
        .find(|label| !pressed.contains(*label))
    {
        return Err(format!("--press {missing:?}: no control carries it"));
    }
    Ok(())
}

/// The views no layout showed a node of: a view its window draws off a call, the system draws
/// for its kind (the default view of a resource, a row of a list, a place on the canvas), or a
/// host shows by name (a screen that takes nothing, which the check opens, [`open_views`]) is
/// not one.
fn unreached_views(apps: &[App], shown: &[Shown]) -> Vec<Unreached> {
    let drawn: Shown = shown.iter().flatten().cloned().collect();
    let called: BTreeSet<(String, String)> = apps
        .iter()
        .flat_map(|held| {
            // A view a package declares as what its kind opens — `default view v for T`, `row view
            // v for T`, `place view v for T` — is called by the system itself, so nothing has to
            // name it.
            let declared = held
                .meta
                .default_views
                .iter()
                .chain(&held.meta.row_views)
                .chain(&held.meta.place_views)
                .filter_map(|declared| {
                    let view = held
                        .meta
                        .views
                        .iter()
                        .find(|view| view.id == declared.view)?;
                    Some((held.meta.app.clone(), view.name.clone()))
                });
            held.sources
                .called()
                .map(|(app, view)| (app.to_owned(), view.to_owned()))
                .chain(declared)
        })
        .collect();
    let mut out = Vec::new();
    for held in apps {
        for view in &held.meta.views {
            if view.presents.is_some() || view.row_of.is_some() {
                continue;
            }
            let key = (held.meta.app.clone(), view.name.clone());
            if drawn.contains(&key) {
                continue;
            }
            out.push(Unreached {
                view: format!("{} › {}", held.short, view.name),
                called: called.contains(&key),
            });
        }
    }
    out
}

/// Stage the person the run lays its windows out for, in place of whatever locale this machine runs
/// in: every window's language is chosen from theirs, so the same sources lay out in the same words
/// on every machine ([`Options::locale`]).
fn stage_person_locale(locale: &str) -> Result<(), String> {
    let language = tilo_process::environment::parse_posix_locale(locale)
        .ok_or_else(|| format!("--locale {locale}: names no language (write `en-US`)"))?;
    let mut environment = tilo_process::environment::environment().as_ref().clone();
    environment.locale = Some(language.to_string());
    tilo_process::environment::install_environment(environment);
    Ok(())
}

/// The columns whose header the window's view declares: a title that names what the column shows,
/// or buttons that act on it.
fn declared_headers(shell: &tilo_app_window::UiNode) -> Vec<&'static str> {
    use tilo_render_protocol::patch::Value;
    [
        ("sidebar", "sidebar_title", "sidebar_actions"),
        ("overview", "overview_title", "overview_actions"),
        ("content", "title", "actions"),
    ]
    .into_iter()
    .filter(|(_, title, actions)| {
        let named = match shell.props.get(*title) {
            Some(Value::Text(title)) => !title.trim().is_empty(),
            Some(_) => true,
            None => false,
        };
        named
            || shell.children.iter().any(|child| {
                child.kind == "part"
                    && child.props.get("name") == Some(&Value::text(*actions))
                    && !child.children.is_empty()
            })
    })
    .map(|(column, ..)| column)
    .collect()
}

/// One control in a column's header row: what the person reads at the top of the column,
/// and what pressing it does there.
#[cfg(test)]
#[derive(Clone, Debug)]
pub(crate) struct RowControl {
    pub name: String,
    pub id: String,
    pub rect: [f64; 4],
    pub primary: bool,
    pub menu: bool,
    /// It says its words beside its icon, rather than shown as its icon alone.
    pub labelled: bool,
}

/// The controls in the window's header rows, in reading order: every button whose middle
/// stands in a column's header row where the person reaches it.
#[cfg(test)]
pub(crate) fn header_row_controls(staged: &stage::Staged, viewport: (u32, u32)) -> Vec<RowControl> {
    fn walk(
        node: &ui::headless::InspectionNode,
        rows: &[ui::kurbo::Rect],
        root: &dyn tilo_ui_base::widget::Widget,
        viewport: (u32, u32),
        out: &mut Vec<RowControl>,
    ) {
        let [x, y, w, h] = node.rect;
        let middle = ui::kurbo::Point::new(x + w / 2.0, y + h / 2.0);
        if let Some(access) = &node.semantics
            && access.role == tilo_ui_base::a11y::Role::Button
            && reachable(node, viewport)
            && rows.iter().any(|row| row.contains(middle))
        {
            let id = node.test_id.clone().unwrap_or_default();
            let primary =
                tilo_ui_base::widget::find_test_id(root, &id).is_some_and(is_primary_button);
            out.push(RowControl {
                name: access.name.clone().unwrap_or_default(),
                menu: node.widget == "OverflowButton",
                id,
                rect: node.rect,
                primary,
                labelled: w > h * 1.5,
            });
            return;
        }
        for child in &node.children {
            walk(child, rows, root, viewport, out);
        }
    }
    fn is_primary_button(widget: &dyn tilo_ui_base::widget::Widget) -> bool {
        if let Some(button) = widget
            .as_any()
            .and_then(|any| any.downcast_ref::<ui::widget::Button>())
        {
            return button.is_primary();
        }
        let mut found = false;
        widget.for_each_child(&mut |child| found = found || is_primary_button(child));
        found
    }
    let harness = staged.harness();
    let rows: Vec<ui::kurbo::Rect> = harness
        .column_frames()
        .iter()
        .map(|frame| frame.header)
        .filter(|header| header.height() > 0.0)
        .collect();
    let mut out = Vec::new();
    walk(
        &harness.inspection().root,
        &rows,
        harness.root_widget(),
        viewport,
        &mut out,
    );
    out
}

/// The names of the buttons the window draws where the person reaches them.
fn button_names(node: &ui::headless::InspectionNode, viewport: (u32, u32), out: &mut Vec<String>) {
    if let Some(access) = &node.semantics
        && access.role == tilo_ui_base::a11y::Role::Button
        && reachable(node, viewport)
        && let Some(name) = access.name.as_ref().filter(|name| !name.is_empty())
    {
        out.push(name.clone());
    }
    for child in &node.children {
        button_names(child, viewport, out);
    }
}

/// The words the window showed from a catalogue in a language other than the one it speaks, since
/// the last frame was read: one problem per message, at the window.
fn mixed_language() -> Vec<audit::Problem> {
    let language = tilo_ui_base::i18n::I18nManager::window_locale();
    let found = tilo_ui_base::i18n::I18nManager::foreign_words();
    tilo_ui_base::i18n::I18nManager::clear_foreign_words();
    found
        .into_iter()
        .map(|(key, locale)| audit::Problem {
            kind: Kind::MixedLanguage,
            keys: Vec::new(),
            widget: "Message".to_owned(),
            id: Some(key.clone()),
            detail: format!(
                "`{key}` reads the {locale} catalogue in a window that speaks {language}"
            ),
        })
        .collect()
}

/// One app of the check, read once: its metadata and
/// its views' source, which every unit of its work
/// shares.
struct App {
    dir: PathBuf,
    short: String,
    meta: tilolang::PackageMeta,
    sources: source::SourceMap,
    roots: Vec<Option<String>>,
    /// Its packages compiled, as a window loads them: a worker copies them into the window it lays
    /// out rather than compiling them again, which takes seconds to run.
    compiled: Vec<tilolang_tilo::tilolang::Compiled>,
    /// The packages it uses as metadata rather than as a package directory: the native apps' (each
    /// a committed description) and the platform's, which every window of it loads too.
    platform: Vec<tilolang_tilo::tilolang::PackageMeta>,
}

impl App {
    fn read(dir: &Path, search: &[PathBuf], root: Option<&str>) -> Result<Self, String> {
        let mut search = search.to_vec();
        search.extend(dir.parent().map(Path::to_path_buf));
        let (built, platform) = build(dir, &search)?;
        let sources = source::SourceMap::new(&built);
        let meta = built.last().ok_or("no package")?.compiled.meta.clone();
        let short = meta.app.rsplit('.').next().unwrap_or(&meta.app).to_owned();
        let compiled = built.into_iter().map(|built| built.compiled).collect();
        Ok(Self {
            dir: dir.to_path_buf(),
            short,
            roots: layers_of(&meta, root),
            meta,
            sources,
            compiled,
            platform,
        })
    }
}

/// The identity of a finding while it is gathered: kind, app, view, node, widget.
type Identity = (Kind, String, String, Option<String>, String);

/// The views some layout showed, as `(app, view name)`: a view drawn in any app's layout is laid
/// out there, so it is reached for the package that defines it (Today draws Chat's Unread place).
type Shown = BTreeSet<(String, String)>;

/// Whether `shown` holds the view `name` of `app`.
fn shows(shown: &Shown, app: &str, name: &str) -> bool {
    shown.contains(&(app.to_owned(), name.to_owned()))
}

/// One scenario of one app on one layer, the window laid out every way people lay one out.
struct Scene {
    app: usize,
    root: Option<String>,
    scenario: String,
}

impl Scene {
    /// The units every scene has: the scenario as staged, its primary actions pressed, and what a
    /// row it opened shows a person holding the window.
    fn units(&self) -> [Unit; 3] {
        [
            Unit {
                scene: self.at(),
                what: What::Scenario,
            },
            Unit {
                scene: self.at(),
                what: What::Press,
            },
            Unit {
                scene: self.at(),
                what: What::Push,
            },
        ]
    }

    /// The views nothing showed yet, opened on the scenario whose resources they take, with what
    /// every scene before this one showed.
    fn opens(&self, shown: &Shown) -> Unit {
        Unit {
            scene: self.at(),
            what: What::Opens {
                shown: shown.clone(),
            },
        }
    }

    fn at(&self) -> At {
        At {
            app: self.app,
            root: self.root.clone(),
            scenario: self.scenario.clone(),
        }
    }

    /// Whether opening a view on this scenario may show one nothing showed yet: the views of types
    /// it did not open, the views the system shows on surfaces of its own, and the screens a host
    /// opens by name.
    fn needs_opens(&self, apps: &[App], shown: &Shown) -> bool {
        let Some(app) = apps.get(self.app) else {
            return false;
        };
        let meta = &app.meta;
        let default = meta.default_views.iter().any(|default| {
            !meta.roots.contains(&default.ty)
                && meta
                    .views
                    .iter()
                    .find(|v| v.id == default.view)
                    .is_some_and(|v| !shows(shown, &meta.app, &v.name))
        });
        default
            || switch_views(meta)
                .iter()
                .any(|(view, _)| !shows(shown, &meta.app, &view.name))
            || surface_views(meta).iter().any(|view| {
                !shows(shown, &meta.app, &view.name)
                    && view
                        .params
                        .first()
                        .is_some_and(|param| meta.types.iter().any(|t| t.name == param.ty))
                    && view.params.len() == 1
            })
            // A page behind the window's own list of destinations is reached only by choosing it,
            // so the check chooses each while a view of the package still stands unshown.
            || (app.sources.navigates()
                && meta
                    .offered_views()
                    .any(|view| !shows(shown, &meta.app, &view.name)))
            // A screen a host shows by name (`view phone()`) takes nothing and no code calls it:
            // the check opens it as the window's own while it stands unshown.
            || meta.views.iter().any(|view| {
                view.params.is_empty()
                    && (names_screen(&self.scenario, &view.name)
                        || (!a_scenario_names(meta, &view.name)
                            && !shows(shown, &meta.app, &view.name)))
                    && !app
                        .sources
                        .called()
                        .any(|(app, name)| app == meta.app && name == view.name)
            })
    }
}

/// Whether the scenario `scenario` names the screen `view`: its name holds the view's name in
/// words, so a scenario called "the phone, signed out" stages the `phone` screen a host shows.
fn names_screen(scenario: &str, view: &str) -> bool {
    let words = |text: &str| -> Vec<String> {
        text.split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .map(str::to_lowercase)
            .collect()
    };
    let said = words(scenario);
    let wanted = words(&view.replace('_', " "));
    !wanted.is_empty()
        && said
            .windows(wanted.len())
            .any(|window| window == wanted.as_slice())
}

/// Whether a scenario of the package names the host screen `view`: where one does, the screen is
/// laid out on those and on no other, so each scenario stages a screen of its own.
fn a_scenario_names(meta: &tilolang::PackageMeta, view: &str) -> bool {
    meta.scenarios
        .iter()
        .any(|scenario| names_screen(&scenario.name, view))
}

/// Where a unit of work runs: one scenario of one app on one layer.
#[derive(Clone)]
struct At {
    app: usize,
    root: Option<String>,
    scenario: String,
}

/// One unit of work: a window staged from one scenario and what is done with it.
#[derive(Clone)]
struct Unit {
    scene: At,
    what: What,
}

#[derive(Clone)]
enum What {
    Scenario,
    /// The scenario with the views nothing showed yet opened on it, laid out every way.
    Opens {
        shown: Shown,
    },
    Press,
    /// The scenario at every size it is held in the hand, with the row it opened tapped as a finger
    /// taps it, or the surface it draws on tapped where nothing stands: the window a person comes to
    /// after choosing what to read, or after pointing at a place of their own.
    Push,
}

/// What a staging costs against one layout of a window: the scenario is made and written down once,
/// at several layouts' cost, before any layout runs.
const STAGING: usize = 6;

impl Unit {
    /// What this unit of work is, for the person reading why it did not run: the app, the window it
    /// stages and what it does with it.
    fn named(&self, apps: &[App]) -> String {
        let app = apps
            .get(self.scene.app)
            .map_or("an app that did not read", |app| app.short.as_str());
        let what = match &self.what {
            What::Scenario => "staged",
            What::Opens { .. } => "with the views no layout showed opened on it",
            What::Press => "with each primary action pressed",
            What::Push => "held in the hand, the row or the surface tapped",
        };
        let window = on_layer(&self.scene.scenario, self.scene.root.as_deref());
        format!("{app}: the scenario `{window}` {what}")
    }

    /// What this unit will cost, in layouts of a window: every window it stages and every layout it
    /// reads, so the units run costliest first.
    fn weight(&self, apps: &[App]) -> usize {
        let layouts = layouts().len();
        let sizes = SIZES.len();
        match &self.what {
            // The scenario's window, and the views it opens on it: each one laid out every way.
            What::Scenario => layouts.saturating_add(STAGING),
            What::Opens { .. } => layouts.saturating_mul(2).saturating_add(STAGING),
            // One window at each size, each pressed: only the sizes whose window carries the
            // control are pressed again, of which none is known here.
            What::Press => {
                let labels = apps
                    .get(self.scene.app)
                    .map_or(0, |app| primary_labels(&app.meta).len());
                let pressed = labels.saturating_mul(sizes).min(sizes);
                sizes
                    .saturating_add((pressed.saturating_add(1)).saturating_mul(STAGING))
                    .saturating_add(pressed)
            }
            // A window at each size held in the hand, the row or surface tapped, and the sizes'
            // layouts read after the tap and after the window's way back.
            What::Push => {
                let touch = SIZES.iter().filter(|size| size.touch).count();
                touch
                    .saturating_mul(STAGING + 1 + 2 * 2)
                    .saturating_add(layouts)
            }
        }
    }
}

#[derive(Default)]
struct Found {
    gathered: BTreeMap<Identity, Finding>,
    layouts: usize,
    shown: Shown,
    /// The `--press` labels a control carried in this unit of work.
    pressed: BTreeSet<String>,
    work: stage::Work,
    /// What the unit dumped (`--dump`): the toolkit's inspection of each layout at the dump size,
    /// held until every unit has run so the page reads in the order the work does.
    dump: String,
}

/// Add what `found` gathered to `into`, as a further layout of the app shows the same.
fn merge(into: &mut BTreeMap<Identity, Finding>, found: BTreeMap<Identity, Finding>) {
    for (identity, finding) in found {
        let Some(held) = into.get_mut(&identity) else {
            into.insert(identity, finding);
            continue;
        };
        held.count = held.count.saturating_add(finding.count);
        for layout in finding.layouts {
            if !held.layouts.contains(&layout) {
                held.layouts.push(layout);
            }
        }
    }
}

/// The app in `dir` compiled after the packages it uses, found in `search`; and the packages it
/// uses as metadata rather than as a package directory: the native apps' (each a committed
/// description, `surface.toml`) and the platform's (the device's, Settings'): [`crate::compile`].
fn build(
    dir: &Path,
    search: &[PathBuf],
) -> Result<
    (
        Vec<tilolang::source::BuiltPackage>,
        Vec<tilolang_tilo::tilolang::PackageMeta>,
    ),
    String,
> {
    crate::compile(dir, search, &mut tilolang::OsEntropy::default()).map_err(|e| e.to_string())
}

/// The stack a worker lays a window out on: a window's tree is as deep as the documents it shows,
/// and a rich-text editor inside a row held open needs more room than a thread's default two
/// megabytes, sized for a plain thread's work.
pub(crate) const WORKER_STACK: usize = 32 * 1024 * 1024;

/// Start `worker` on a thread of the scope with a window's stack, and say why one could not start:
/// the work it would have taken then has nobody to run it.
fn spawn_worker<'scope, 'env>(
    scope: &'scope std::thread::Scope<'scope, 'env>,
    worker: impl FnOnce() + Send + 'scope,
) -> Result<(), String> {
    std::thread::Builder::new()
        .stack_size(WORKER_STACK)
        .spawn_scoped(scope, worker)
        .map(drop)
        .map_err(|error| format!("a worker could not start: {error}"))
}

/// Wait for any other layout check on this machine to finish, and hold the machine until this one
/// has: the lock lives as long as the returned guard.
///
/// What it protects is the machine's cores, not anything a check writes: a check fans out to as
/// many workers as it has units of work (up to the machine's parallelism), and the tests are
/// processes of their own that nextest starts together, so without the lock several of their pools
/// would run at once and their sum is what the machine would have to run. The lock is per user and
/// machine (`XDG_RUNTIME_DIR`), so every check in a container meets every other.
fn machine_lock() -> Result<advisory_lock::FileLock, String> {
    use advisory_lock::{FileLock, LockMode, TryLockError};
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, Into::into);
    let path = std::path::Path::new(&dir).join("tilolang-layout-check.lock");
    match FileLock::try_open(&path, LockMode::Exclusive) {
        Ok(lock) => Ok(lock),
        Err(TryLockError::WouldBlock) => {
            eprintln!("waiting for another layout check on this machine to finish");
            FileLock::open(&path, LockMode::Exclusive)
                .map_err(|error| format!("{}: {error}", path.display()))
        }
        Err(TryLockError::Error(error)) => Err(format!("{}: {error}", path.display())),
    }
}

/// The workers a run of `units` starts: one per unit of work, and never more than the machine lays
/// out at once. What a check spends is its units — one scenario laid out every way — so a check
/// that names one app starts a pool that app's scenarios fill rather than one per core; a worker
/// takes the next unit when it is free, and a worker over the units would only wait on `next`
/// holding a worker's stack.
fn workers(units: usize) -> usize {
    let at_once = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    at_once.min(units)
}

/// Run every unit of `units` at the same time, each on a worker of its own, and return what each
/// found in the order given.
fn run(apps: &[App], units: &[Unit], options: &Options) -> Result<Vec<Found>, String> {
    if units.is_empty() {
        return Ok(Vec::new());
    }
    let workers = workers(units.len());
    let taken: Vec<Mutex<Option<Result<Found, String>>>> =
        units.iter().map(|_| Mutex::new(None)).collect();
    // The costliest units first: a worker takes the next one when it is free, so the sweep ends
    // when its heaviest unit does, and leaving those for last would hold every other worker
    // waiting.
    let mut order: Vec<usize> = (0..units.len()).collect();
    order.sort_by_key(|&at| std::cmp::Reverse(units.get(at).map_or(0, |unit| unit.weight(apps))));
    let next = AtomicUsize::new(0);
    let stopped = AtomicBool::new(false);
    let unstarted: Mutex<Option<String>> = Mutex::new(None);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let started = spawn_worker(scope, || {
                tilo_app_window::i18n::ensure();
                let mut worker = Worker {
                    apps,
                    options,
                    text: tilo_ui_base::TextRenderer::new(),
                    loaded: None,
                };
                while !stopped.load(Ordering::Relaxed) {
                    let at = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&at) = order.get(at) else {
                        break;
                    };
                    let Some(unit) = units.get(at) else {
                        break;
                    };
                    let found = worker.check(unit);
                    if found.is_err() {
                        stopped.store(true, Ordering::Relaxed);
                    }
                    if let Some(result) = taken.get(at) {
                        *result.lock().unwrap_or_else(PoisonError::into_inner) = Some(found);
                    }
                }
            });
            if let Err(failure) = started {
                stopped.store(true, Ordering::Relaxed);
                *unstarted.lock().unwrap_or_else(PoisonError::into_inner) = Some(failure);
                break;
            }
        }
    });
    if let Some(failure) = unstarted
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner)
    {
        return Err(failure);
    }
    let mut out = Vec::with_capacity(units.len());
    // A unit that failed stops the sweep, so the units after it never ran: the reason is the
    // failure, named after the unit that hit it, never the first unit the stop left empty.
    let mut failed: Option<String> = None;
    let mut unrun: Option<String> = None;
    for (at, result) in taken.into_iter().enumerate() {
        let held = result.into_inner().unwrap_or_else(PoisonError::into_inner);
        let named = || units.get(at).map(|unit| unit.named(apps));
        match held {
            Some(Ok(found)) => out.push(found),
            Some(Err(why)) => {
                if failed.is_none() {
                    let unit = named().unwrap_or_default();
                    failed = Some(format!("{unit}: {why}"));
                }
            }
            None => {
                if unrun.is_none() {
                    let unit = named().unwrap_or_default();
                    unrun = Some(format!("{unit}: it did not run"));
                }
            }
        }
    }
    if let Some(failure) = failed.or(unrun) {
        return Err(failure);
    }
    Ok(out)
}

/// Run `chain` for each app of `busy` at once, each on a
/// worker of its own, and write what it found in that app's
/// result.
fn opens_together(
    busy: &[usize],
    results: &[Mutex<Option<Result<Opened, String>>>],
    chain: &(impl Fn(usize) -> Result<Opened, String> + Sync),
) -> Result<(), String> {
    let unstarted: Mutex<Option<String>> = Mutex::new(None);
    std::thread::scope(|scope| {
        for &app in busy {
            let Some(result) = results.get(app) else {
                continue;
            };
            let started = spawn_worker(scope, move || {
                tilo_app_window::i18n::ensure();
                *result.lock().unwrap_or_else(PoisonError::into_inner) = Some(chain(app));
            });
            if let Err(failure) = started {
                *unstarted.lock().unwrap_or_else(PoisonError::into_inner) = Some(failure);
                break;
            }
        }
    });
    match unstarted
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner)
    {
        Some(failure) => Err(failure),
        None => Ok(()),
    }
}

/// What opening the views of one scene found, under the index of the scene it was staged on.
type Opened = Vec<(usize, Found)>;

/// Run the views no layout showed yet, on the scene whose resources
/// they take, for every app: an app's scenes one after another, each
/// seeing what the ones before it showed, and the apps at the same
/// time.
fn run_opens(
    apps: &[App],
    scenes: &[Scene],
    phase: &[Found],
    options: &Options,
) -> Result<(Vec<Found>, Vec<Shown>), String> {
    let mut per_app: Vec<Vec<usize>> = vec![Vec::new(); apps.len()];
    for (index, scene) in scenes.iter().enumerate() {
        if let Some(held) = per_app.get_mut(scene.app) {
            held.push(index);
        }
    }
    let results: Vec<Mutex<Option<Result<Opened, String>>>> =
        per_app.iter().map(|_| Mutex::new(None)).collect();
    let busy: Vec<usize> = per_app
        .iter()
        .enumerate()
        .filter(|(_, indices)| !indices.is_empty())
        .map(|(app, _)| app)
        .collect();
    let chain = |app: usize| -> Result<Opened, String> {
        let Some(indices) = per_app.get(app) else {
            return Ok(Vec::new());
        };
        let mut worker = Worker {
            apps,
            options,
            text: tilo_ui_base::TextRenderer::new(),
            loaded: None,
        };
        // What the scenes before this one showed, and this one itself has shown of its own.
        let mut shown = BTreeSet::new();
        let mut opened = Vec::new();
        for &index in indices {
            let Some(scene) = scenes.get(index) else {
                continue;
            };
            let [scenario, press, push] = scene_units(phase, index);
            let mut seed = shown.clone();
            if let Some(scenario) = scenario {
                seed.extend(scenario.shown.iter().cloned());
            }
            let found = if scene.needs_opens(apps, &seed) {
                let unit = scene.opens(&seed);
                let found = worker
                    .check(&unit)
                    .map_err(|why| format!("{}: {why}", unit.named(apps)))?;
                shown.extend(found.shown.iter().cloned());
                found
            } else {
                Found::default()
            };
            if let Some(press) = press {
                shown.extend(press.shown.iter().cloned());
            }
            if let Some(push) = push {
                shown.extend(push.shown.iter().cloned());
            }
            opened.push((index, found));
        }
        Ok(opened)
    };
    opens_together(&busy, &results, &chain)?;
    let mut opened: Vec<Found> = (0..scenes.len()).map(|_| Found::default()).collect();
    let mut shown: Vec<Shown> = vec![Shown::new(); apps.len()];
    for (app, result) in results.into_iter().enumerate() {
        let Some(entries) = result.into_inner().unwrap_or_else(PoisonError::into_inner) else {
            continue;
        };
        let Some(shown) = shown.get_mut(app) else {
            continue;
        };
        for (index, found) in entries? {
            shown.extend(found.shown.iter().cloned());
            if let Some(entry) = opened.get_mut(index) {
                *entry = found;
            }
        }
    }
    for (index, scene) in scenes.iter().enumerate() {
        let [scenario, press, push] = scene_units(phase, index);
        let Some(held) = shown.get_mut(scene.app) else {
            continue;
        };
        for found in [scenario, press, push].into_iter().flatten() {
            held.extend(found.shown.iter().cloned());
        }
    }
    Ok((opened, shown))
}

/// The units every scene has in the first phase, in the order the check ran them.
fn scene_units(phase: &[Found], index: usize) -> [Option<&Found>; 3] {
    let mut held = [None, None, None];
    for (entry, found) in held
        .iter_mut()
        .zip(phase.chunks(3).nth(index).unwrap_or_default())
    {
        *entry = Some(found);
    }
    held
}

/// One thread's check: the apps it reads, the packages
/// of the app it is in, and the text it measures with.
struct Worker<'a> {
    apps: &'a [App],
    options: &'a Options,
    text: tilo_ui_base::TextRenderer,
    loaded: Option<(usize, Rc<stage::Packages>)>,
}

impl Worker<'_> {
    fn check(&mut self, unit: &Unit) -> Result<Found, String> {
        let packages = self.packages(unit.scene.app)?;
        let Some(app) = self.apps.get(unit.scene.app) else {
            return Err(format!("no app {}", unit.scene.app));
        };
        let mut check = AppCheck {
            dir: &app.dir,
            short: &app.short,
            meta: &app.meta,
            sources: &app.sources,
            dump: self.options.dump,
            shots: self.options.shots.as_deref(),
            bar_at_rest: self.options.bar_at_rest,
            shot_height: self.options.shot_height,
            themes: &self.options.themes,
            states: &self.options.states,
            keys: &self.options.keys,
            hovers: &self.options.hovers,
            presses: &self.options.presses,
            gathered: BTreeMap::new(),
            text: &mut self.text,
            ran: 0,
            shown: match &unit.what {
                What::Opens { shown } => shown.clone(),
                What::Scenario | What::Press | What::Push => BTreeSet::new(),
            },
            pressed: BTreeSet::new(),
            work: stage::Work::default(),
            dumped: String::new(),
        };
        check.unit(&packages, unit)?;
        Ok(Found {
            gathered: check.gathered,
            layouts: check.ran,
            shown: check.shown,
            pressed: check.pressed,
            work: check.work,
            dump: check.dumped,
        })
    }

    /// `app`'s packages as the window loads them, copied on the first unit of it this worker runs
    /// and again when the worker moves to another app.
    fn packages(&mut self, app: usize) -> Result<Rc<stage::Packages>, String> {
        if let Some((held, packages)) = &self.loaded
            && *held == app
        {
            return Ok(Rc::clone(packages));
        }
        let Some(held) = self.apps.get(app) else {
            return Err(format!("no app {app}"));
        };
        // A copy of what the app's own read compiled: the window loads packages of its own, and
        // compiling them again would spend seconds of every worker's first unit on it.
        let packages = Rc::new(stage::Packages::new(
            Rc::new(held.compiled.clone()),
            held.platform.clone(),
        ));
        self.loaded = Some((app, Rc::clone(&packages)));
        Ok(packages)
    }
}

fn layers_of(meta: &tilolang::PackageMeta, root: Option<&str>) -> Vec<Option<String>> {
    if let Some(name) = root {
        return vec![Some(name.to_owned())];
    }
    let mut layers: Vec<Option<String>> = vec![None];
    // Every other root the package declares, by the name `--root` takes: a named root's own, and
    // the type's name for a root with none.
    let others: BTreeSet<&str> = meta
        .named_roots
        .iter()
        .filter(|r| !r.device && meta.roots.first() != Some(&r.ty))
        .map(|r| r.name.as_str())
        .collect();
    for ty in &meta.roots {
        // A root each device keeps its own (`device root readings: Readings`) is the app's state
        // on the device, never a window's: it opens no layer of its own.
        if meta.device_roots.contains(ty) {
            continue;
        }
        let Some(declared) = meta.types.iter().find(|t| &t.id == ty) else {
            continue;
        };
        if meta.named_roots.iter().any(|r| &r.ty == ty) || meta.roots.first() == Some(ty) {
            // Already laid out: the first root is the
            // window's own, and a named root has its name
            // below.
            continue;
        }
        layers.push(Some(declared.name.clone()));
    }
    for name in &others {
        layers.push(Some((*name).to_owned()));
    }
    layers
}

/// What the check calls `scenario` laid out on `root`: the scenario's name on the window's own
/// layer, and the layer's name after it on any other.
fn on_layer(scenario: &str, root: Option<&str>) -> String {
    root.map_or_else(
        || scenario.to_owned(),
        |root| format!("{scenario} on {root}"),
    )
}

/// The resources the rows of the window's lists show: a row is keyed by the view that draws it,
/// which ends in the item it draws.
fn row_items(
    rows: &[(String, tilo_app_window::UiNode)],
    resources: &[ResourceRef],
) -> Vec<ResourceRef> {
    rows.iter()
        .filter_map(|(key, _)| {
            resources
                .iter()
                .find(|resource| key.ends_with(&resource.id))
                .cloned()
        })
        .collect()
}

/// A resource of type `ty` to lay a page of it out on: one the window's lists hold, so the page is
/// one a person meets from the list, or else any the window names.
fn resource_of(listed: &[ResourceRef], named: &[ResourceRef], ty: &str) -> Option<ResourceRef> {
    listed
        .iter()
        .find(|resource| resource.ty == ty)
        .cloned()
        .or_else(|| named.iter().find(|resource| resource.ty == ty).cloned())
}

/// The resource [`resource_of`] picks for `ty` off `staged`'s window: the first row of that type
/// the window's lists show, or else the first the window names.
fn listed_of(staged: &mut stage::Staged, ty: &str) -> Option<ResourceRef> {
    let rows = staged.rows();
    let named = staged.resources();
    resource_of(&row_items(&rows, &named), &named, ty)
}

/// The views of `meta` the system shows on surfaces of its own: a type's row and place views, and
/// each action's proposal view, once each.
fn surface_views(meta: &tilolang::PackageMeta) -> Vec<&tilolang::meta::ViewMeta> {
    let ids: BTreeSet<&str> = meta
        .row_views
        .iter()
        .map(|row| row.view.as_str())
        .chain(meta.place_views.iter().map(|place| place.view.as_str()))
        .chain(
            meta.actions
                .iter()
                .filter_map(|action| action.proposal.as_ref().map(|p| p.view.as_str())),
        )
        .collect();
    meta.views
        .iter()
        .filter(|view| ids.contains(view.id.as_str()))
        .collect()
}

/// The views beside the one a type's windows open on, each with the type it takes: what the
/// window's bar offers as a switch between them (a project as a board beside its list).
fn switch_views(
    meta: &tilolang::PackageMeta,
) -> Vec<(&tilolang::meta::ViewMeta, &tilolang::meta::TypeMeta)> {
    let mut out = Vec::new();
    for default in &meta.default_views {
        let Some(ty) = meta.types.iter().find(|t| t.id == default.ty) else {
            continue;
        };
        out.extend(
            meta.views_of(&ty.id)
                .into_iter()
                .filter(|id| *id != default.view)
                .filter_map(|id| {
                    let view = meta.views.iter().find(|view| view.id == id)?;
                    Some((view, ty))
                }),
        );
    }
    out
}

/// What the window calls each of `meta`'s primary actions, once each.
fn primary_labels(meta: &tilolang::PackageMeta) -> Vec<&str> {
    let mut labels: Vec<&str> = meta
        .actions
        .iter()
        .filter(|action| action.role.as_deref() == Some("primary") && !action.scenario)
        .map(|action| action.label.as_str())
        .collect();
    labels.sort_unstable();
    labels.dedup();
    labels
}

/// The test id of the first control under `node` named `label` that a person can press: a button,
/// or a row of a list or a tree, which a press opens where it stands.
fn button_named(node: &ui::headless::InspectionNode, label: &str) -> Option<String> {
    let pressable = node.semantics.as_ref().is_some_and(|access| {
        matches!(
            access.role,
            tilo_ui_base::a11y::Role::Button
                | tilo_ui_base::a11y::Role::ToggleButton
                | tilo_ui_base::a11y::Role::ListItem
                | tilo_ui_base::a11y::Role::TreeItem
        ) && !access.state.disabled
            && access.answers_to(label)
    });
    if pressable
        && node.rect[2] > 0.0
        && node.rect[3] > 0.0
        && let Some(id) = &node.test_id
    {
        return Some(id.clone());
    }
    node.children
        .iter()
        .find_map(|child| button_named(child, label))
}

/// Where the drawn row `label` of a list or a tree stands under `node`, when the row names no
/// control of its own to press: a sidebar's place, which a press at where it stands chooses.
fn drawn_row_named(node: &ui::headless::InspectionNode, label: &str) -> Option<ui::kurbo::Rect> {
    let row = node.semantics.as_ref().is_some_and(|access| {
        matches!(
            access.role,
            tilo_ui_base::a11y::Role::ListItem | tilo_ui_base::a11y::Role::TreeItem
        ) && !access.state.disabled
            && access.answers_to(label)
    });
    if row && node.rect[2] > 0.0 && node.rect[3] > 0.0 {
        return Some(ui::kurbo::Rect::new(
            node.rect[0],
            node.rect[1],
            node.rect[0] + node.rect[2],
            node.rect[1] + node.rect[3],
        ));
    }
    node.children
        .iter()
        .find_map(|child| drawn_row_named(child, label))
}

/// How a press by its words reaches a window: the control of that name, the box a widget publishes
/// of its own, or where a menu row of that name stands — a row is painted by the menu that shows
/// it, so only a press at where it stands reaches it.
enum Pressing {
    /// A control, and whether a click reaches it rather than its semantic action.
    Control(String, bool),
    /// A control a widget publishes as a semantic child of
    /// its own: the widget holding it, and the child's
    /// target.
    Child(String, String),
    At(ui::kurbo::Rect),
}

/// The box a widget publishes as a control of its own, named `label`: the box beside a row's title,
/// which the row draws apart from its own words so that a person or a tool reaches the box and not
/// the row (`Mark Buy oat milk done`).
fn checked_control_named(
    node: &ui::headless::InspectionNode,
    label: &str,
) -> Option<(String, String)> {
    let target = node.semantics.as_ref().and_then(|access| {
        access.children.iter().find_map(|child| {
            let presses = matches!(
                child.role,
                tilo_ui_base::a11y::Role::Checkbox
                    | tilo_ui_base::a11y::Role::Switch
                    | tilo_ui_base::a11y::Role::RadioButton
            );
            if presses && !child.state.disabled && child.name.as_deref() == Some(label) {
                child.action_target.clone()
            } else {
                None
            }
        })
    });
    if let Some(target) = target
        && let Some(id) = &node.test_id
    {
        return Some((id.clone(), target));
    }
    node.children
        .iter()
        .find_map(|child| checked_control_named(child, label))
}

/// The box the row `label` of a menu stands at, under `node`, when the window shows one.
fn painted_row_named(node: &ui::headless::InspectionNode, label: &str) -> Option<ui::kurbo::Rect> {
    node.semantics
        .as_ref()
        .and_then(|access| row_named(access, label))
        .or_else(|| {
            node.children
                .iter()
                .find_map(|child| painted_row_named(child, label))
        })
}

/// The box of the row `label` among what `access`
/// publishes, a menu's rows being its children's
/// children: a menu row by its name, a table row by its name or the words of its first cell.
fn row_named(access: &tilo_ui_base::a11y::AccessNode, label: &str) -> Option<ui::kurbo::Rect> {
    for child in &access.children {
        if child.role == tilo_ui_base::a11y::Role::MenuItem
            && !child.state.disabled
            && child.name.as_deref() == Some(label)
            && let Some(bounds) = child.bounds
        {
            return Some(bounds.to_rect());
        }
        if child.role == tilo_ui_base::a11y::Role::Row
            && !child.state.disabled
            && let Some(bounds) = child.bounds
            && (child.name.as_deref() == Some(label)
                || child.children.first().is_some_and(|cell| {
                    cell.value.as_deref() == Some(label) || cell.name.as_deref() == Some(label)
                }))
        {
            return Some(bounds.to_rect());
        }
        if let Some(bounds) = row_named(child, label) {
            return Some(bounds);
        }
    }
    None
}

/// The test id of the first widget under `node` that publishes a button named `label` as a control
/// it paints itself (an empty state's action): the widget is what is activated.
fn painted_button_named(node: &ui::headless::InspectionNode, label: &str) -> Option<String> {
    let painted = node.semantics.as_ref().is_some_and(|access| {
        access.children.iter().any(|child| {
            matches!(
                child.role,
                tilo_ui_base::a11y::Role::Button | tilo_ui_base::a11y::Role::ToggleButton
            ) && !child.state.disabled
                && child.name.as_deref() == Some(label)
        })
    });
    if painted && let Some(id) = &node.test_id {
        return Some(id.clone());
    }
    node.children
        .iter()
        .find_map(|child| painted_button_named(child, label))
}

struct AppCheck<'a> {
    dir: &'a Path,
    short: &'a str,
    meta: &'a tilolang::PackageMeta,
    sources: &'a source::SourceMap,
    dump: Option<Dump>,
    shots: Option<&'a Path>,
    /// Take the shots with the window's bar at rest ([`Options::bar_at_rest`]).
    bar_at_rest: bool,
    /// The height the shots are taken at ([`Options::shot_height`]).
    shot_height: Option<u32>,
    themes: &'a [String],
    states: &'a [String],
    keys: &'a [String],
    hovers: &'a [String],
    presses: &'a [String],
    gathered: BTreeMap<Identity, Finding>,
    text: &'a mut tilo_ui_base::TextRenderer,
    ran: usize,
    shown: Shown,
    /// The `--press` labels a control carried, so a label no layout carries at all is the one the
    /// run reports: a mark a window shows in one scenario and not another is pressed where it
    /// stands.
    pressed: BTreeSet<String>,
    work: stage::Work,
    /// The `--dump` page of this unit of work, as [`Found::dump`].
    dumped: String,
}

impl AppCheck<'_> {
    fn unit(&mut self, packages: &Rc<stage::Packages>, unit: &Unit) -> Result<(), String> {
        let title = on_layer(&unit.scene.scenario, unit.scene.root.as_deref());
        match unit.what {
            What::Scenario => {
                let mut staged = self.stage(packages, unit)?;
                self.read(&mut staged, &title)
            }
            What::Opens { .. } => {
                let mut staged = self.stage(packages, unit)?;
                self.open_views(&mut staged, &title, &unit.scene.scenario)?;
                self.choose_destinations(packages, unit, &title)
            }
            What::Press => self.press(packages, unit),
            What::Push => self.push(packages, unit),
        }
    }

    /// The scenario as staged, laid out once at the smallest window so a window's first frame is
    /// drawn before its layouts are read.
    fn stage(&self, packages: &Rc<stage::Packages>, unit: &Unit) -> Result<stage::Staged, String> {
        let first = layouts().first().copied().ok_or("no layouts")?;
        self.stage_at(packages, unit, (first.size.width, first.size.height))
    }

    /// The scenario as staged, drawn at `viewport` rather than at the smallest window.
    fn stage_at(
        &self,
        packages: &Rc<stage::Packages>,
        unit: &Unit,
        viewport: (u32, u32),
    ) -> Result<stage::Staged, String> {
        stage::Staged::over(
            &self.meta.app,
            packages,
            &unit.scene.scenario,
            viewport,
            tilo_ui_base::Theme::light(),
            unit.scene.root.as_deref(),
            self.states,
        )
    }

    /// The views of types the scenario did not open, on a resource its window names; the views
    /// the system shows on surfaces of its own rather than in a window of the app: a type's row
    /// view (a search result, a row of an agent's answer), a type's place view (what a place on the
    /// canvas shows) and an action's proposal view (the card asking to run it), on a resource of
    /// the type they take; and the screens a host shows by name, which take nothing.
    fn open_views(
        &mut self,
        staged: &mut stage::Staged,
        title: &str,
        scenario: &str,
    ) -> Result<(), String> {
        // A page a person meets from a list is one of that list's own rows, so a view staged over
        // the list opens a resource the list holds where it holds one, and a resource of a type the
        // list does not show where it holds none.
        for default in &self.meta.default_views {
            let Some(view) = self.meta.views.iter().find(|v| v.id == default.view) else {
                continue;
            };
            if shows(&self.shown, &self.meta.app, &view.name)
                || self.meta.roots.contains(&default.ty)
            {
                continue;
            }
            let Some(resource) = listed_of(staged, &default.ty) else {
                continue;
            };
            staged.open(resource.clone());
            self.read(staged, &format!("{title}, then a {} opened", view.name))?;
            // An app whose rows open where they stand (a task opened in its list) answers the
            // record's own action API with its place: its page is still a view the app draws, from the
            // row's menu, so the run opens it by its name as that menu row does.
            if !shows(&self.shown, &self.meta.app, &view.name) {
                staged.open_with(&resource, &view.id);
                self.read(
                    staged,
                    &format!("{title}, then a {} opened on its own", view.name),
                )?;
            }
        }
        for view in surface_views(self.meta) {
            if shows(&self.shown, &self.meta.app, &view.name) {
                continue;
            }
            let [param] = view.params.as_slice() else {
                continue;
            };
            // A parameter names its type as written; a resource carries the type's id.
            let Some(ty) = self.meta.types.iter().find(|t| t.name == param.ty) else {
                continue;
            };
            let Some(resource) = listed_of(staged, &ty.id) else {
                continue;
            };
            // The window shows the item first, as a person's path does: a named view opened over a
            // window showing another item stages a screen nobody reaches.
            staged.open(resource.clone());
            staged.open_with(&resource, &view.id);
            self.read(staged, &format!("{title}, then {} shown", view.name))?;
        }
        // The views a type's bar switches to beside the one it opens on: a project laid out as a
        // board as well as the list it opens on.
        let meta = self.meta;
        for (view, ty) in switch_views(meta) {
            if shows(&self.shown, &self.meta.app, &view.name) {
                continue;
            }
            let Some(resource) = listed_of(staged, &ty.id) else {
                continue;
            };
            // The same: the item first, then the named view over it.
            staged.open(resource.clone());
            staged.open_with(&resource, &view.id);
            self.read(staged, &format!("{title}, then {} shown", view.name))?;
        }
        // A view that takes nothing and no code of the package calls is a screen its host shows
        // by name rather than a resource's: a phone's shell (`view phone()`). Its host opens it
        // as the window's own, over no resource, which is what the check lays out here: on each
        // scenario that names it (so each state it stages is a screen of its own), and once on
        // another where nothing showed it yet.
        for view in &meta.views {
            if !view.params.is_empty() {
                continue;
            }
            let named = names_screen(scenario, &view.name);
            if !named
                && (a_scenario_names(meta, &view.name) || shows(&self.shown, &meta.app, &view.name))
            {
                continue;
            }
            let called = self
                .sources
                .called()
                .any(|(app, name)| app == meta.app && name == view.name);
            if called {
                continue;
            }
            staged.open_screen(&view.id);
            self.read(staged, &format!("{title}, then {} alone", view.name))?;
        }
        Ok(())
    }

    /// The pages the window's own list of destinations leads to, each chosen the way a person
    /// chooses it: a window shows one page of its list, so the views behind the others are reached
    /// only by picking one (`ui.NavItem` in a sidebar). Each is read from the scenario as staged,
    /// against the whole list drawn at the widest window, and laid out every way once it shows a
    /// view no layout showed: a destination that shows what the window already stands on adds
    /// nothing.
    fn choose_destinations(
        &mut self,
        packages: &Rc<stage::Packages>,
        unit: &Unit,
        title: &str,
    ) -> Result<(), String> {
        let Some(widest) = layouts().last().copied() else {
            return Ok(());
        };
        let viewport = (widest.size.width, widest.size.height);
        let mut staged = self.stage_at(packages, unit, viewport)?;
        staged.lay_out(
            viewport,
            widest.theme(),
            widest.input(),
            widest.display_scale,
        )?;
        let choices = destinations(&staged.harness().inspection().root, viewport);
        for (label, at) in choices {
            let mut chosen = self.stage_at(packages, unit, viewport)?;
            chosen.lay_out(
                viewport,
                widest.theme(),
                widest.input(),
                widest.display_scale,
            )?;
            chosen.press_at(at.0, at.1)?;
            chosen.lay_out(
                viewport,
                widest.theme(),
                widest.input(),
                widest.display_scale,
            )?;
            if !self.shows_unseen(&mut chosen) {
                continue;
            }
            self.read(&mut chosen, &format!("{title}, then {label} chosen"))?;
        }
        Ok(())
    }

    /// Whether the window shows a view no layout has shown yet: a widget's, or one the app sent that
    /// drew nothing, which only its key names.
    fn shows_unseen(&self, staged: &mut stage::Staged) -> bool {
        fn unseen(key: &str, sources: &source::SourceMap, shown: &Shown) -> bool {
            sources
                .views_in(key)
                .any(|(app, view)| !shows(shown, app, view))
        }
        fn inspecting(
            node: &ui::headless::InspectionNode,
            sources: &source::SourceMap,
            shown: &Shown,
        ) -> bool {
            node.test_id
                .as_deref()
                .is_some_and(|key| unseen(key, sources, shown))
                || node
                    .children
                    .iter()
                    .any(|child| inspecting(child, sources, shown))
        }
        fn composing(
            node: &tilo_app_window::UiNode,
            sources: &source::SourceMap,
            shown: &Shown,
        ) -> bool {
            unseen(&node.key, sources, shown)
                || node
                    .children
                    .iter()
                    .any(|child| composing(child, sources, shown))
        }
        if inspecting(
            &staged.harness().inspection().root,
            self.sources,
            &self.shown,
        ) {
            return true;
        }
        staged
            .composed()
            .is_some_and(|composed| composing(&composed, self.sources, &self.shown))
    }

    /// What the window shows once a person pressed one of its primary actions (a new note opened
    /// beside the list, its header row laid out again around it): at each size, from the scenario
    /// as staged, pressed as a person presses (a tap on a touchscreen, a click otherwise) and with
    /// the motion it starts run out, read as it stands then, before anything lays the window out
    /// afresh.
    fn press(&mut self, packages: &Rc<stage::Packages>, unit: &Unit) -> Result<(), String> {
        let labels = primary_labels(self.meta);
        if labels.is_empty() {
            return Ok(());
        }
        // Which of the window's primary controls each size carries, read off one window laid out at
        // every size in turn: a control the window does not show at a size is one nobody presses
        // there, and a window staged for it would be a window thrown away.
        let carried = self.controls_of_each_size(packages, unit, &labels)?;
        for (index, label) in labels.iter().enumerate() {
            for (at, size) in SIZES.iter().enumerate() {
                if !carried.get(at).is_some_and(|here| here.contains(&index)) {
                    continue;
                }
                let label = *label;
                let layout = Layout {
                    size: *size,
                    display_scale: 1.0,
                    text_scale: 1.0,
                };
                let viewport = (size.width, size.height);
                let mut staged = stage::Staged::over(
                    &self.meta.app,
                    packages,
                    &unit.scene.scenario,
                    viewport,
                    layout.theme(),
                    unit.scene.root.as_deref(),
                    self.states,
                )?;
                staged.lay_out_moving(viewport, layout.theme(), layout.input())?;
                let found = {
                    let root = &staged.harness().inspection().root;
                    button_named(root, label)
                        .map(|id| (id, true))
                        .or_else(|| painted_button_named(root, label).map(|id| (id, false)))
                };
                let Some((id, tapped)) = found else {
                    continue;
                };
                if tapped {
                    staged.press(&id, size.touch)?;
                } else {
                    staged.press_action(&id)?;
                }
                self.audit(
                    &mut staged,
                    layout,
                    &format!(
                        "{}, then {label} pressed",
                        on_layer(&unit.scene.scenario, unit.scene.root.as_deref())
                    ),
                )?;
            }
        }
        Ok(())
    }

    /// The indices of `labels` the window carries at each size, each read off the one window laid
    /// out at that size: the scenario as staged, with the motion a press reaches it by.
    fn controls_of_each_size(
        &self,
        packages: &Rc<stage::Packages>,
        unit: &Unit,
        labels: &[&str],
    ) -> Result<Vec<BTreeSet<usize>>, String> {
        let Some(first) = SIZES.first().copied() else {
            return Ok(Vec::new());
        };
        let mut staged = stage::Staged::over(
            &self.meta.app,
            packages,
            &unit.scene.scenario,
            (first.width, first.height),
            tilo_ui_base::Theme::light(),
            unit.scene.root.as_deref(),
            self.states,
        )?;
        let mut carried = Vec::with_capacity(SIZES.len());
        for size in SIZES {
            let layout = Layout {
                size,
                display_scale: 1.0,
                text_scale: 1.0,
            };
            staged.lay_out_moving((size.width, size.height), layout.theme(), layout.input())?;
            let root = &staged.harness().inspection().root;
            carried.push(
                labels
                    .iter()
                    .enumerate()
                    .filter(|(_, label)| {
                        button_named(root, label).is_some()
                            || painted_button_named(root, label).is_some()
                    })
                    .map(|(index, _)| index)
                    .collect(),
            );
        }
        Ok(carried)
    }

    /// What a person holding the window comes to after choosing what to read: at every size it is
    /// held in the hand, a row of the list the window shows tapped as a finger taps it, which
    /// pushes what the row opens in front of that list.
    fn push(&mut self, packages: &Rc<stage::Packages>, unit: &Unit) -> Result<(), String> {
        let all = layouts();
        for size in SIZES.iter().filter(|size| size.touch) {
            let viewport = (size.width, size.height);
            let of_size: Vec<Layout> = all
                .iter()
                .copied()
                .filter(|layout| layout.size == *size)
                .collect();
            let Some(first) = of_size.first().copied() else {
                continue;
            };
            let mut staged = stage::Staged::over(
                &self.meta.app,
                packages,
                &unit.scene.scenario,
                viewport,
                first.theme(),
                unit.scene.root.as_deref(),
                self.states,
            )?;
            let mut layer = on_layer(&unit.scene.scenario, unit.scene.root.as_deref());
            staged.lay_out(viewport, first.theme(), first.input(), first.display_scale)?;
            // What the app opened stands in front of the list it came from: a
            // scenario that returns a resource shows that resource's page, and a
            // row of the page is the page's own, where tapping it chooses what it
            // shows.
            let mut page_in_front = self.scenario_returns_a_resource(&unit.scene.scenario);
            let mut tapped = if page_in_front {
                None
            } else {
                tappable_row(&staged, viewport)
            };
            if tapped.is_none() {
                // A window whose list stands over what it shows as a drawer brings it forward by
                // its sidebar toggle, which the app's page never covers: the list is a screen of
                // the window's, not a page pressed over the page it opened.
                if sidebar_toggle(&staged, viewport) {
                    staged.press_shell(ui::widget::WINDOW_SIDEBAR)?;
                    layer = format!("{layer}, then the sidebar");
                    for layout in of_size.iter().copied() {
                        staged.lay_out(
                            (layout.size.width, layout.size.height),
                            layout.theme(),
                            layout.input(),
                            layout.display_scale,
                        )?;
                        self.audit(&mut staged, layout, &layer)?;
                    }
                    // A row of the list arrives in front of what the window showed, which goes
                    // behind it: the tap that opens the row brings that page back.
                    tapped = unchosen_row(&staged, viewport);
                } else if back_chip(&staged, viewport) {
                    let shown = staged.front_target();
                    staged.press_shell(ui::widget::WINDOW_BACK)?;
                    layer = format!("{layer}, then Back pressed");
                    for layout in of_size.iter().copied() {
                        staged.lay_out(
                            (layout.size.width, layout.size.height),
                            layout.theme(),
                            layout.input(),
                            layout.display_scale,
                        )?;
                        self.audit(&mut staged, layout, &layer)?;
                    }
                    // A window that kept what it showed has no list behind it to tap.
                    page_in_front = page_in_front && staged.front_target() == shown;
                    if !page_in_front {
                        tapped = tappable_row(&staged, viewport);
                    }
                }
            }
            // The row the window keeps chosen, or else the first of the list: what a person reaches
            // for, which opens in front of the list as the app declares.
            let Some(tapped) = tapped else {
                self.tap_surface(&mut staged, viewport, &layer, &of_size)?;
                continue;
            };
            let before = staged.front_target();
            staged.tap_at(tapped)?;
            // The tap shows what its row opens where the list was; a row whose tap opens nothing
            // leaves the list showing, and the shot's name says so.
            let opened = staged
                .front_target()
                .filter(|shown| Some(shown) != before.as_ref());
            let label = match opened {
                Some(shown) => format!("{layer}, then a {} in front", self.view_opening(&shown)),
                None => format!("{layer}, then the row tapped"),
            };
            for layout in of_size {
                staged.lay_out(
                    (layout.size.width, layout.size.height),
                    layout.theme(),
                    layout.input(),
                    layout.display_scale,
                )?;
                self.audit(&mut staged, layout, &label)?;
            }
        }
        Ok(())
    }

    /// What a window that lists nothing to open shows when a person points into it instead: a finger
    /// taps the surface its places stand on, where nothing drawn on it stands, and the page the
    /// window shows at that point is read as the tap's page.
    fn tap_surface(
        &mut self,
        staged: &mut stage::Staged,
        viewport: (u32, u32),
        layer: &str,
        of_size: &[Layout],
    ) -> Result<(), String> {
        let mut tapped = None;
        for (name, at) in surface_presses(staged, viewport) {
            staged.tap_at(at)?;
            if self.shows_unseen(staged) {
                tapped = Some(name);
                break;
            }
        }
        let Some(name) = tapped else {
            return Ok(());
        };
        let label = format!("{layer}, then {name} tapped");
        for layout in of_size.iter().copied() {
            staged.lay_out(
                (layout.size.width, layout.size.height),
                layout.theme(),
                layout.input(),
                layout.display_scale,
            )?;
            self.audit(staged, layout, &label)?;
        }
        Ok(())
    }

    /// Whether the scenario the window was staged from returned a resource of its own (`scenario "a
    /// busy week" -> Task`): the window shows that resource's page in front of the list the scenario
    /// made.
    fn scenario_returns_a_resource(&self, scenario: &str) -> bool {
        self.meta
            .scenarios
            .iter()
            .find(|entry| entry.name == scenario)
            .and_then(|entry| {
                self.meta
                    .actions
                    .iter()
                    .find(|action| action.id == entry.action)
            })
            .is_some_and(|action| action.result.is_some())
    }

    /// The view a window opens `target` in: its type's
    /// default view, which is what a row of it opens.
    fn view_opening(&self, target: &ResourceRef) -> String {
        self.meta
            .default_views
            .iter()
            .find(|default| default.ty == target.ty)
            .and_then(|default| {
                let view = self
                    .meta
                    .views
                    .iter()
                    .find(|view| view.id == default.view)?;
                Some(view.name.clone())
            })
            .unwrap_or_else(|| target.ty.clone())
    }

    /// Lay `staged` out at every layout and gather what is wrong, as staged by `label`.
    fn read(&mut self, staged: &mut stage::Staged, label: &str) -> Result<(), String> {
        // The keys reach the window at the size it is read at, where a shot shows them; a check
        // taken without a dump size reads them at its first layout.
        let read_at = self.dump.map(Dump::size).or_else(|| {
            layouts()
                .first()
                .map(|layout| (layout.size.width, layout.size.height))
        });
        let mut pressed = false;
        for layout in layouts() {
            let viewport = (layout.size.width, layout.size.height);
            staged.lay_out(
                viewport,
                layout.theme(),
                layout.input(),
                layout.display_scale,
            )?;
            if !pressed
                && Some(viewport) == read_at
                && (!self.keys.is_empty() || !self.presses.is_empty() || !self.hovers.is_empty())
            {
                // What the person sees before they touch it, where the run takes shots: a press
                // leaves the window as it opened something, and both are what a person reads.
                if let Some(dir) = self.shots
                    && self.dump.is_some_and(|dump| dump.size() == viewport)
                    && (layout.display_scale - 1.0).abs() < f64::EPSILON
                    && (layout.text_scale - 1.0).abs() < f32::EPSILON
                {
                    self.shoot(staged, layout, label, dir, false)?;
                }
                // What a person opens first, then what they reach by keyboard: the window is laid
                // out as they leave it, with a popover hanging and a control wearing its ring.
                for press in self.presses {
                    let found = {
                        let root = &staged.harness().inspection().root;
                        button_named(root, press)
                            .map(|id| Pressing::Control(id, true))
                            .or_else(|| {
                                painted_button_named(root, press)
                                    .map(|id| Pressing::Control(id, false))
                            })
                            .or_else(|| {
                                checked_control_named(root, press)
                                    .map(|(id, target)| Pressing::Child(id, target))
                            })
                            .or_else(|| drawn_row_named(root, press).map(Pressing::At))
                            .or_else(|| painted_row_named(root, press).map(Pressing::At))
                    };
                    let Some(pressing) = found else {
                        continue;
                    };
                    self.pressed.insert(press.clone());
                    match pressing {
                        Pressing::Control(id, clicked) if clicked => {
                            staged.press(&id, layout.size.touch)?;
                        }
                        Pressing::Control(id, _) => staged.press_action(&id)?,
                        Pressing::Child(id, target) => staged.press_child(&id, &target)?,
                        Pressing::At(bounds) => {
                            let center = bounds.center();
                            staged.press_at(center.x, center.y)?;
                        }
                    }
                    // What the press opened — a menu over the window, a popover beside a control —
                    // is laid out before the next word is looked for.
                    staged.freshen()?;
                }
                for key in self.keys {
                    staged.key(key);
                }
                self.rest_pointer(staged)?;
                staged.lay_out(
                    viewport,
                    layout.theme(),
                    layout.input(),
                    layout.display_scale,
                )?;
                pressed = true;
                // The shot and the findings as the press leaves the window, under a label of their
                // own: the window it opened is never the one the scenario staged.
                let opened = if self.presses.is_empty() {
                    label.to_owned()
                } else {
                    format!("{label}, then {} pressed", self.presses.join(" and "))
                };
                self.audit(staged, layout, &opened)?;
                continue;
            }
            self.audit(staged, layout, label)?;
        }
        Ok(())
    }

    /// Record the bar and the frame `audit` read: written to the dump when `dump` asks for this
    /// frame (`found` says a finding was read from it), with a capture at plain scale whenever
    /// shots were asked for at its size.
    fn record_frame(
        &mut self,
        dump: Dump,
        staged: &mut stage::Staged,
        layout: Layout,
        label: &str,
        bar: &ui::window_toolbar::BarLayout,
        inspection: &ui::headless::InspectionSnapshot,
        found: bool,
    ) -> Result<(), String> {
        let viewport = (layout.size.width, layout.size.height);
        if dump.prints(viewport, found) {
            let json = serde_json::to_string_pretty(bar).map_err(|e| e.to_string())?;
            let _ = writeln!(
                self.dumped,
                "== {} {label} at {layout} — the bar\n{json}",
                self.meta.app
            );
            let json = serde_json::to_string_pretty(inspection).map_err(|e| e.to_string())?;
            let _ = writeln!(
                self.dumped,
                "== {} {label} at {layout} — the frame\n{json}",
                self.meta.app
            );
        }
        if let Some(dir) = self.shots
            && dump.size() == viewport
            && (layout.display_scale - 1.0).abs() < f64::EPSILON
            && (layout.text_scale - 1.0).abs() < f32::EPSILON
        {
            self.shoot(staged, layout, label, dir, true)?;
        }
        Ok(())
    }

    /// Gather what is wrong with `staged` as it is laid out now, at `layout`, as staged by `label`.
    fn audit(
        &mut self,
        staged: &mut stage::Staged,
        layout: Layout,
        label: &str,
    ) -> Result<(), String> {
        let theme = layout.theme();
        self.text.set_text_scale(theme.text_scale);
        self.text.apply_typography(&theme.tokens.typography);
        self.require_built(staged, label)?;
        self.ran = self.ran.saturating_add(1);
        let inspection = staged.harness().inspection();
        // The bar the desktop draws over the frame: the app's own parts merged with the window's
        // navigation, its one "…" and its controls.
        let bar = staged.harness_mut().window_bar_layout();
        let problems = self.problems(staged, layout, &inspection, &bar);
        if let Some(dump) = self.dump {
            let found = !problems.is_empty();
            self.record_frame(dump, staged, layout, label, &bar, &inspection, found)?;
        }
        self.gather_all(problems, layout, label);
        self.work.add(staged.take_work());
        Ok(())
    }

    /// Refuse a window the app left broken: one that refused
    /// what the app sent, or whose components did not build.
    fn require_built(&self, staged: &mut stage::Staged, label: &str) -> Result<(), String> {
        if let Some(why) = staged.failed() {
            return Err(format!(
                "{}: {label}: the window refused what the app sent: {why}",
                self.dir.display(),
            ));
        }
        let broken = staged.broken_components();
        if !broken.is_empty() {
            return Err(format!(
                "{}: {label}: components of the view did not build:\n  {}",
                self.dir.display(),
                broken.join("\n  ")
            ));
        }
        Ok(())
    }

    /// What is wrong with the window: its frame's, its views'
    /// and its bar's, each read off where the window shows it.
    fn problems(
        &mut self,
        staged: &mut stage::Staged,
        layout: Layout,
        inspection: &ui::headless::InspectionSnapshot,
        bar: &ui::window_toolbar::BarLayout,
    ) -> Vec<audit::Problem> {
        let viewport = (layout.size.width, layout.size.height);
        self.note_shown(&inspection.root);
        // A view that ran but drew nothing has no widget; what the app sent still names it.
        let mut declared = ux::Declared::default();
        if let Some(composed) = staged.composed() {
            self.note_shown_row(&composed);
            declared.read(&composed);
        }
        let rows = staged.rows();
        for (_, row) in &rows {
            self.note_shown_row(row);
            declared.read(row);
        }
        let min_target = layout
            .size
            .touch
            .then(|| f64::from(layout.input().minimum_target()));
        let mut problems =
            audit::inspect(&inspection.root, viewport, min_target, layout.display_scale);
        // A heading keeps one rhythm to the rows it names: the room a finger's target takes stands
        // over its line, never between the heading and its first row.
        problems.extend(audit::heading_rooms(
            &inspection.root,
            &layout.theme(),
            min_target,
        ));
        // A page's body keeps the room a control beside its heading leads it to only where the
        // column has that room: a phone's screen starts it on the gutter.
        problems.extend(audit::indented_bodies(
            &inspection.root,
            &layout.theme(),
            min_target,
        ));
        problems.extend(audit::clipped_text(
            staged.harness_mut().root_widget_mut(),
            self.text,
            viewport,
        ));
        problems.extend(ux::inspect(
            &inspection.root,
            viewport,
            &declared,
            tilo_ui_base::chrome::disclosure_gutter(&layout.theme()),
            bar.controls
                .iter()
                .find(|control| control.id == "window-toolbar-location")
                .map(|control| control.name.as_str())
                .filter(|name| !name.trim().is_empty()),
        ));
        problems.extend(mixed_language());
        // The bar the harness offers is the one bar of one app
        // window, over its frame: what its app declares for it.
        let one_window = self.meta.roots.len() <= 1 && self.meta.named_roots.is_empty();
        if one_window {
            problems.extend(audit::bar(bar));
        }
        let (bar_height, _) = staged.harness().frame_extent(viewport);
        // The words the bar paints itself stand over the frame's own, which is laid out under the
        // bar: both are read where the window shows them.
        problems.extend(audit::text_on_text(
            &inspection.root,
            viewport,
            &bar.labels,
            bar_height,
        ));
        // A window shows one bar: a header row under it is a second one.
        problems.extend(audit::stacked_header_rows(
            bar_height,
            staged.harness().window_header_row_height(),
        ));
        // The bar's primary action is read off the parts this window declared, never the process's:
        // windows laid out side by side in one process keep their bars apart.
        if one_window {
            problems.extend(audit::bar_primary(
                staged.harness().window_toolbar().as_ref(),
                bar,
            ));
        }
        // Each column's header row stands at rest, as the app's own: what names a column and acts
        // on what it shows is on the window at every time, never only once the window's bar is out,
        // and the bar never offers one of those actions again.
        if one_window {
            let declared = staged
                .composed()
                .map(|composed| declared_headers(&composed))
                .unwrap_or_default();
            problems.extend(audit::column_headers_at_rest(
                &declared,
                &staged.harness().column_frames(),
                f64::from(viewport.0),
            ));
            let mut buttons = Vec::new();
            button_names(&inspection.root, viewport, &mut buttons);
            problems.extend(audit::offered_twice(bar, &buttons));
        }
        for problem in &mut problems {
            // A list draws a row as one widget keyed by the row: what it is about is the row view's
            // node, the deepest the row's tree holds.
            if let Some(row) = problem
                .id
                .as_ref()
                .and_then(|id| rows.iter().find(|(key, _)| key == id))
                .map(|(_, row)| row)
            {
                let mut inner = Vec::new();
                row_keys(row, &mut inner);
                inner.reverse();
                inner.append(&mut problem.keys);
                problem.keys = inner;
                problem.id = None;
            }
        }
        problems
    }

    /// Gather each problem as `label` staged it.
    fn gather_all(&mut self, problems: Vec<audit::Problem>, layout: Layout, label: &str) {
        for problem in problems {
            self.gather(&problem, layout, label);
        }
    }

    /// Write the window as `label` leaves it at `layout` to
    /// `dir`, light and dark, and lay it out again as it
    /// was.
    /// Rest the pointer where `--hover` puts it, in the window as it is laid out now.
    fn rest_pointer(&self, staged: &mut stage::Staged) -> Result<(), String> {
        for hover in self.hovers {
            let (x, y) = hover
                .split_once(',')
                .and_then(|(x, y)| Some((x.trim().parse().ok()?, y.trim().parse().ok()?)))
                .ok_or_else(|| format!("--hover {hover}: a point such as 640,480"))?;
            staged.hover(x, y);
        }
        Ok(())
    }

    /// Shoot the window as it stands, in each theme; `hovering` rests the pointer where `--hover`
    /// puts it in the window at the shot's size, which may be taller than the one it was read at.
    fn shoot(
        &self,
        staged: &mut stage::Staged,
        layout: Layout,
        label: &str,
        dir: &Path,
        hovering: bool,
    ) -> Result<(), String> {
        let slug: String = label
            .chars()
            .map(|c| {
                if c.is_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect();
        let shot = (
            layout.size.width,
            self.shot_height
                .map_or(layout.size.height, |tall| tall.max(layout.size.height)),
        );
        let viewport = (layout.size.width, layout.size.height);
        let stock = [String::new()];
        let themes = if self.themes.is_empty() {
            &stock[..]
        } else {
            self.themes
        };
        for family in themes {
            let pair = if family.is_empty() {
                theme_pair("default")?
            } else {
                theme_pair(family)?
            };
            for (name, theme) in pair {
                staged.lay_out(shot, theme, layout.input(), 1.0)?;
                if hovering {
                    self.rest_pointer(staged)?;
                }
                if self.bar_at_rest {
                    staged.harness_mut().present_window_bar_at_rest();
                }
                let file = if family.is_empty() {
                    dir.join(format!("{}-{slug}-{name}.png", self.short))
                } else {
                    dir.join(format!(
                        "{}-{slug}-{}-{name}.png",
                        self.short,
                        family_slug(family)
                    ))
                };
                staged.harness_mut().render_png(&file);
            }
        }
        staged.lay_out(viewport, layout.theme(), layout.input(), 1.0)
    }

    /// Remember the views shown in `key`: the view the node is of, and every view whose call holds
    /// it — of this app or of any other whose views this window draws.
    fn note_key(&mut self, key: &str) {
        for (app, view) in self.sources.views_in(key) {
            self.shown.insert((app.to_owned(), view.to_owned()));
        }
    }

    /// Remember the views whose nodes `node` and what it holds show.
    fn note_shown(&mut self, node: &ui::headless::InspectionNode) {
        if let Some(id) = node.test_id.as_deref() {
            self.note_key(id);
        }
        for child in &node.children {
            self.note_shown(child);
        }
    }

    /// Remember the views a tree the app sent (a row's, the window's) is of.
    fn note_shown_row(&mut self, node: &tilo_app_window::UiNode) {
        self.note_key(&node.key);
        for child in &node.children {
            self.note_shown_row(child);
        }
    }

    fn gather(&mut self, problem: &audit::Problem, layout: Layout, label: &str) {
        // The window's own parts (the shell's Back button) have test ids of their own, not the
        // compiler's: they are shown as the window's, by that id.
        let chrome = problem
            .id
            .as_ref()
            .filter(|id| self.sources.origin(id).is_none());
        let origin = match chrome {
            Some(_) => None,
            None => self.sources.origin_of(&self.meta.app, &problem.keys),
        };
        let (view, node) = origin.as_ref().map_or_else(
            || ("(window)".to_owned(), chrome.cloned()),
            |o| (o.view.clone(), o.node.clone()),
        );
        let identity = (
            problem.kind,
            self.short.to_owned(),
            view.clone(),
            node.clone(),
            problem.widget.clone(),
        );
        let shown = layout.to_string();
        let finding = self.gathered.entry(identity).or_insert_with(|| Finding {
            kind: problem.kind,
            app: self.short.to_owned(),
            view,
            node,
            widget: problem.widget.clone(),
            file: origin.as_ref().map(|o| o.span.file.clone()),
            line: origin.as_ref().map(|o| o.span.line),
            column: origin.as_ref().map(|o| o.span.column),
            detail: problem.detail.clone(),
            scenario: label.to_owned(),
            layouts: Vec::new(),
            count: 0,
            id: String::new(),
            allowed: false,
        });
        finding.count = finding.count.saturating_add(1);
        if !finding.layouts.contains(&shown) {
            finding.layouts.push(shown);
        }
    }
}

/// The keys of a row's tree, outermost first, leaving out its parts (a menu, a swipe).
fn row_keys(node: &tilo_app_window::UiNode, out: &mut Vec<String>) {
    if node.kind == "part" {
        return;
    }
    out.push(node.key.clone());
    for child in &node.children {
        row_keys(child, out);
    }
}

/// The report as people read it: one line per finding, at its place in the source.
#[must_use]
pub fn render(report: &Report) -> String {
    let mut out = String::new();
    for finding in &report.findings {
        let place = match (&finding.file, finding.line, finding.column) {
            (Some(file), Some(line), Some(column)) => format!("{file}:{line}:{column}: "),
            _ => String::new(),
        };
        let first = finding.layouts.first().map_or("", String::as_str);
        let more = finding.layouts.len().saturating_sub(1);
        let _ = writeln!(
            out,
            "{place}{}{}: {} (at {first}{}; scenario {:?})",
            if finding.allowed { "allowed " } else { "" },
            finding.id,
            finding.detail,
            if more > 0 {
                format!(" and {more} more layouts")
            } else {
                String::new()
            },
            finding.scenario,
        );
    }
    for stale in &report.stale {
        let _ = writeln!(
            out,
            "note: the allow-list names `{stale}`, which no longer shows: take it out"
        );
    }
    let named = |views: Vec<&Unreached>| {
        views
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<String>>()
            .join(", ")
    };
    let dead: Vec<&Unreached> = report
        .unreached
        .iter()
        .filter(|view| !view.called)
        .collect();
    if !dead.is_empty() {
        let one = dead.len() == 1;
        let _ = writeln!(
            out,
            "note: no view calls {}: nothing can draw {}, so delete {}",
            named(dead),
            if one { "it" } else { "them" },
            if one { "it" } else { "them" },
        );
    }
    let missed: Vec<&Unreached> = report.unreached.iter().filter(|view| view.called).collect();
    if !missed.is_empty() {
        let one = missed.len() == 1;
        let _ = writeln!(
            out,
            "note: no scenario reaches {}: a view calls {}, so a scenario must reach the view that draws {}",
            named(missed),
            if one { "it" } else { "each" },
            if one { "it" } else { "them" },
        );
    }
    let new = report.new_findings().count();
    let _ = writeln!(
        out,
        "{} findings ({new} not allowed) in {} layouts, {}.{} s",
        report.findings.len(),
        report.layouts,
        report.elapsed_ms / 1000,
        report.elapsed_ms % 1000 / 100,
    );
    // What the layouts spent, counted by the toolkit rather than timed: the same sources give the
    // same numbers on any machine, so two runs of a change are compared by them.
    let layout = report.work.layout;
    let _ = writeln!(
        out,
        "layout: {} frames, {} solves in {} passes ({}.{} s), {} region solves and {} kept, {} measures, {} probes in {} copies",
        report.work.frames,
        layout.solves,
        layout.passes,
        layout.solve_nanos / 1_000_000_000,
        layout.solve_nanos % 1_000_000_000 / 100_000_000,
        layout.regions,
        layout.kept,
        layout.measures,
        layout.probes,
        layout.copies,
    );
    out
}

#[cfg(test)]
mod windows;

#[cfg(test)]
mod board_list {
    use std::path::Path;
    use std::rc::Rc;

    use super::stage;
    use tilo_ui_base::widget::Widget;
    use ui::kurbo::Rect;

    const BOARD: &str = "so.tilo.lang.layoutboard";

    fn packages() -> Rc<stage::Packages> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../tools/layout-check/tests/layout_check/board");
        let search = vec![dir.parent().expect("tests/layout_check").to_path_buf()];
        let (built, platform) = super::build(&dir, &search).expect("the board fixture compiles");
        let app = built.last().expect("the fixture").compiled.meta.app.clone();
        assert_eq!(app, BOARD, "the fixture's app id");
        Rc::new(stage::Packages::new(
            Rc::new(built.into_iter().map(|b| b.compiled).collect()),
            platform,
        ))
    }

    /// Every widget under `node` named `name`, in drawing order.
    fn named<'a>(node: &'a dyn Widget, name: &str, out: &mut Vec<&'a dyn Widget>) {
        if node.name() == name {
            out.push(node);
        }
        for child in node.children() {
            named(child, name, out);
        }
    }

    /// Whether `rect` stands whole inside `clip`: a card cut by
    /// the view that scrolls its list is not one a person sees.
    fn inside(clip: Rect, rect: Rect) -> bool {
        /// `layout` places edges on fractions of a pixel.
        const SLACK: f64 = 1.0;
        rect.height() > SLACK
            && rect.x0 >= clip.x0 - SLACK
            && rect.y0 >= clip.y0 - SLACK
            && rect.x1 <= clip.x1 + SLACK
            && rect.y1 <= clip.y1 + SLACK
    }

    /// Whether `node` is a card as a board draws one: the card's own chrome around a body, or the
    /// row a card component showed as its surface ([`ui::widget::CardBody`]), which a board's card
    /// is.
    fn shows_as_card(node: &dyn Widget) -> bool {
        node.name() == "Card"
            || node
                .as_any()
                .and_then(|any| any.downcast_ref::<ui::widget::ListRow>())
                .is_some_and(ui::widget::ListRow::is_card)
    }

    /// The cards `node` holds that stand whole inside `clip`, in drawing order.
    fn whole_cards(node: &dyn Widget, clip: Rect, out: &mut Vec<Rect>) {
        let rect = node.get_rect();
        if shows_as_card(node) && inside(clip, rect) {
            out.push(rect);
        }
        let clip = node.child_clip_rect().map_or(clip, |own| {
            Rect::new(
                clip.x0.max(own.x0),
                clip.y0.max(own.y0),
                clip.x1.min(own.x1),
                clip.y1.min(own.y1),
            )
        });
        for child in node.children() {
            whole_cards(child, clip, out);
        }
    }

    /// A board's page stands the window's height, so its lists are as tall as the window: a list
    /// holding as many cards as the window shows draws every one of them under its heading, and a
    /// list holding more keeps them inside the view that scrolls it.
    #[test]
    fn a_list_draws_every_card_the_window_has_room_for() {
        tilo_app_window::i18n::ensure();
        let packages = packages();
        let staged = stage::Staged::over(
            BOARD,
            &packages,
            "a board",
            (1280, 800),
            tilo_ui_base::Theme::light(),
            None,
            &[],
        )
        .expect("the board stages");
        let root = staged.harness().root_widget();
        let window = root.get_rect();
        let mut pages = Vec::new();
        named(root, "Page", &mut pages);
        assert_eq!(pages.len(), 1, "the board is one page");
        let page = pages[0].get_rect();
        assert!(
            page.y1 <= window.y1 + 1.0,
            "the board's page stands the window's height: {page:?} in {window:?}"
        );
        let mut whole = Vec::new();
        whole_cards(root, window, &mut whole);
        assert!(!whole.is_empty(), "the board draws its cards");
        // Lists stand side by side: the leftmost holds six cards, which the window shows at once,
        // and the rightmost thirty, more than its list has room for.
        let left = whole
            .iter()
            .map(|rect| rect.x0)
            .fold(f64::INFINITY, f64::min);
        let right = whole
            .iter()
            .map(|rect| rect.x0)
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(right > left + 1.0, "the board shows more than one list");
        let list = |x: f64| {
            whole
                .iter()
                .filter(|rect| (rect.x0 - x).abs() < 1.0)
                .count()
        };
        assert_eq!(
            list(left),
            6,
            "the first list draws all six cards the window has room for"
        );
        assert!(
            list(right) < 30,
            "the third list scrolls its thirty cards instead of growing past the window"
        );
    }

    /// The board's sideways scroller under `root`: the list strip it scrolls, the part of that
    /// strip the pane shows, and the lists on the strip in list order.
    fn across(root: &dyn Widget) -> (&ui::widget::Scrollable, Rect, Rect, Vec<Rect>) {
        let mut scrolls = Vec::new();
        named(root, "Scrollable", &mut scrolls);
        let pane = scrolls
            .into_iter()
            .find_map(|scroll| {
                let scroll = scroll.as_any()?.downcast_ref::<ui::widget::Scrollable>()?;
                matches!(
                    scroll.layout_info(),
                    Some(tilo_ui_base::widget::LayoutInfo::Scroll(
                        tilo_ui_base::widget::ScrollLayout {
                            direction: tilo_ui_base::widget::ScrollDirection::Horizontal,
                        }
                    ))
                )
                .then_some(scroll)
            })
            .expect("the board scrolls across");
        let viewed = pane.child_clip_rect().unwrap_or_else(|| pane.get_rect());
        let strip = pane
            .children()
            .first()
            .map(|content| content.get_rect())
            .expect("the board's lists stand on a strip");
        let lists = pane
            .children()
            .iter()
            .flat_map(|content| content.children())
            .map(Widget::get_rect)
            .collect();
        (pane, viewed, strip, lists)
    }

    /// The width a list reads at, which every board gives it (`metrics.board_list_width`).
    fn list_measure() -> f64 {
        f64::from(tilo_ui_base::Theme::light().tokens.metrics.board_list_width)
    }

    /// A pane too narrow for its lists even at a card's measure scrolls them sideways instead of
    /// cutting them off: each list keeps its measure and stands whole on the strip, the strip keeps
    /// the same gutter at each end, every list the viewport shows is whole from its heading to its
    /// foot, and the last list comes whole into view at the end of the scroll.
    #[test]
    fn a_narrow_board_scrolls_its_lists_whole_across_the_pane() {
        tilo_app_window::i18n::ensure();
        let packages = packages();
        let staged = stage::Staged::over(
            BOARD,
            &packages,
            "a board",
            (800, 800),
            tilo_ui_base::Theme::light(),
            None,
            &[],
        )
        .expect("the board stages");
        let root = staged.harness().root_widget();
        let (pane, viewed, strip, lists) = across(root);
        assert_eq!(lists.len(), 3, "three lists: {lists:?}");
        assert!(
            pane.horizontal_overflows(),
            "three lists of {measure} cannot narrow into the 800 pane, so it scrolls them: {strip:?} in {viewed:?}",
            measure = list_measure()
        );
        for list in &lists {
            assert!(
                (list.width() - list_measure()).abs() < 1.0,
                "a list keeps its measure: {list:?} at {measure}",
                measure = list_measure()
            );
            assert!(
                inside(strip, *list),
                "a list stands whole on the strip: {list:?} in {strip:?}"
            );
        }
        let start = lists[0].x0 - strip.x0;
        let end = strip.x1 - lists[2].x1;
        assert!(
            start > 1.0,
            "the first list keeps clear of the strip's edge: {strip:?}"
        );
        assert!(
            (start - end).abs() < 1.0,
            "the lists keep the same gutter at both ends of the strip: {start} before, {end} after"
        );
        // The strip is the pane's whole width and height: it stands inside the page it is shown in,
        // so nothing but the lists' scroll moves it.
        let mut pages = Vec::new();
        named(root, "Page", &mut pages);
        assert_eq!(pages.len(), 1, "the board is one page");
        assert!(
            inside(pages[0].get_rect(), pane.get_rect()),
            "the view that scrolls the board stands inside the page: {:?} in {:?}",
            pane.get_rect(),
            pages[0].get_rect()
        );
        // Every list the pane shows now is whole from its heading to its foot: only the lists
        // scrolled out of view are out of reach until the person scrolls to them.
        let shown: Vec<Rect> = lists
            .iter()
            .copied()
            .filter(|list| list.x1 > viewed.x0 && list.x0 < viewed.x1)
            .collect();
        assert!(!shown.is_empty(), "the pane shows lists: {viewed:?}");
        for list in &shown {
            assert!(
                list.height() > 1.0 && list.y0 >= viewed.y0 - 1.0 && list.y1 <= viewed.y1 + 1.0,
                "a list the pane shows is whole, heading to foot: {list:?} in {viewed:?}"
            );
        }
        // The end of the scroll brings the last list whole into view, less the gutter after it.
        let shift = strip.width() - viewed.width();
        let reached = Rect::new(viewed.x0 + shift, viewed.y0, viewed.x1 + shift, viewed.y1);
        assert!(
            inside(reached, lists[2]),
            "the last list comes whole into view at the end of the scroll: {:?} in {reached:?}",
            lists[2]
        );
    }

    /// A pane wide enough for a board's lists shows them all at rest and offers no sideways scroll:
    /// each keeps its measure, the strip ends inside the pane, and the room the wider pane leaves
    /// open stays beside the lists rather than stretching them.
    #[test]
    fn a_board_with_room_for_its_lists_offers_no_sideways_scroll() {
        tilo_app_window::i18n::ensure();
        let packages = packages();
        let staged = stage::Staged::over(
            BOARD,
            &packages,
            "a board",
            (1280, 800),
            tilo_ui_base::Theme::light(),
            None,
            &[],
        )
        .expect("the board stages");
        let root = staged.harness().root_widget();
        let (pane, viewed, strip, lists) = across(root);
        assert_eq!(lists.len(), 3, "three lists: {lists:?}");
        assert!(
            !pane.horizontal_overflows(),
            "the lists fit the pane, so it scrolls across nothing: {strip:?} in {viewed:?}"
        );
        assert!(
            strip.x1 <= viewed.x1 + 1.0,
            "the strip ends inside the pane: {strip:?} in {viewed:?}"
        );
        for list in &lists {
            assert!(
                (list.width() - list_measure()).abs() < 1.0,
                "a list keeps its measure where the pane has room to spare: {list:?}"
            );
            assert!(
                inside(viewed, *list),
                "every list stands whole in a pane wide enough for it: {list:?} in {viewed:?}"
            );
        }
    }
}

#[cfg(test)]
mod row_press {
    use super::{drawn_row_named, row_named};
    use tilo_ui_base::{a11y::AccessNode, a11y::Role};
    use ui::headless::InspectionNode;
    use ui::kurbo::Rect;

    /// A table row is pressed by the words of its first cell, where it stands.
    #[test]
    fn a_table_row_is_found_by_its_first_cell() {
        let at = Rect::new(0.0, 40.0, 300.0, 68.0);
        let row = AccessNode::new(Role::Row)
            .with_bounds(at)
            .with_children(vec![
                AccessNode::new(Role::Cell).with_value("release.toml"),
                AccessNode::new(Role::Cell).with_value("New"),
            ]);
        let table = AccessNode::new(Role::Table).with_children(vec![row]);
        assert_eq!(row_named(&table, "release.toml"), Some(at));
        assert_eq!(
            row_named(&table, "New"),
            None,
            "only the row's own first words name it"
        );
    }

    fn drawn(
        rect: [f64; 4],
        semantics: AccessNode,
        children: Vec<InspectionNode>,
    ) -> InspectionNode {
        InspectionNode {
            path: String::new(),
            widget: String::new(),
            test_id: None,
            focusable: false,
            rect,
            child_clip: None,
            page_title: None,
            layout: None,
            paint_key: None,
            semantics: Some(semantics),
            children,
        }
    }

    /// A sidebar's place names no control of its own: a press reaches it where it stands.
    #[test]
    fn a_place_of_a_sidebar_is_found_where_it_stands() {
        let place = drawn(
            [8.0, 300.0, 264.0, 30.0],
            AccessNode::new(Role::TreeItem).with_name("Data"),
            Vec::new(),
        );
        let tree = drawn(
            [0.0, 0.0, 280.0, 800.0],
            AccessNode::new(Role::Tree),
            vec![place],
        );
        assert_eq!(
            drawn_row_named(&tree, "Data"),
            Some(Rect::new(8.0, 300.0, 272.0, 330.0))
        );
        assert_eq!(drawn_row_named(&tree, "Media"), None);
    }
}
