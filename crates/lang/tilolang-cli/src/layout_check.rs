//! `tilolang layout-check` (docs/designs/app-language/layout.md, "Checking"): every view of an
//! app, staged from each of its scenarios, laid out headlessly through the UI host's path at
//! the sizes, display scales and text scales people use, and what is wrong with each layout
//! reported at the view's node in its source.
//!
//! Each scenario's window is one instance and one frame pipeline, laid out again for every
//! configuration; nothing is painted or rasterised. Every scenario is a window of its own, so
//! the windows are checked at the same time, one per core.

mod audit;
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
pub use stage::Work;
use tilo_render_protocol::patch::ResourceRef;
use ui::interaction::InteractionEnvironment;

/// A window size people use, in logical pixels, and whether it is held in the hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub width: u32,
    pub height: u32,
    pub touch: bool,
}

/// Phones, a tablet, a half-width window on a portrait monitor, a laptop, a desktop monitor and
/// an ultrawide one.
pub const SIZES: [Size; 7] = [
    Size {
        width: 320,
        height: 568,
        touch: true,
    },
    Size {
        width: 390,
        height: 844,
        touch: true,
    },
    Size {
        width: 768,
        height: 1024,
        touch: true,
    },
    Size {
        width: 960,
        height: 1080,
        touch: false,
    },
    Size {
        width: 1280,
        height: 800,
        touch: false,
    },
    Size {
        width: 1920,
        height: 1080,
        touch: false,
    },
    Size {
        width: 3440,
        height: 1440,
        touch: false,
    },
];

/// Display scales, a fractional one among them: layout is in logical pixels, so these change only
/// where edges snap to device pixels and how input is scaled.
pub const DISPLAY_SCALES: [f64; 3] = [1.0, 1.5, 2.0];

/// The person's text size: as shipped, and enlarged.
pub const TEXT_SCALES: [f32; 2] = [1.0, 1.5];

/// One layout of a window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub size: Size,
    pub display_scale: f64,
    pub text_scale: f32,
}

impl std::fmt::Display for Layout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}×{} @{}x text {}",
            self.size.width, self.size.height, self.display_scale, self.text_scale
        )
    }
}

impl Layout {
    /// The theme of the layout: as shipped, at its text scale.
    #[must_use]
    pub fn theme(&self) -> ui::Theme {
        ui::Theme::light().with_text_scale(self.text_scale)
    }

    /// How a person reaches the window at its size.
    #[must_use]
    pub fn input(&self) -> InteractionEnvironment {
        if self.size.touch {
            InteractionEnvironment::TOUCH
        } else {
            InteractionEnvironment::POINTER
        }
    }
}

/// Every layout the check runs.
#[must_use]
pub fn layouts() -> Vec<Layout> {
    let mut out = Vec::new();
    for size in SIZES {
        for display_scale in DISPLAY_SCALES {
            for text_scale in TEXT_SCALES {
                out.push(Layout {
                    size,
                    display_scale,
                    text_scale,
                });
            }
        }
    }
    out
}

/// One problem of an app, wherever it showed.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Finding {
    pub kind: Kind,
    /// The app's short name (`notes` for `so.tilo.lang.notes`).
    pub app: String,
    /// The view whose node it is, or `(window)` for the window's own parts.
    pub view: String,
    /// The UI call in the view's source, when known.
    pub node: Option<String>,
    /// The toolkit widget it is about.
    pub widget: String,
    pub file: Option<String>,
    pub line: Option<usize>,
    pub column: Option<usize>,
    /// What is wrong, in the first layout it showed in.
    pub detail: String,
    /// The scenario it showed in first.
    pub scenario: String,
    /// The layouts it showed in.
    pub layouts: Vec<String>,
    /// How many times it showed across them (once per row of a list, say).
    pub count: usize,
    /// What an allow-list names it by ([`Finding::id`]).
    pub id: String,
    /// Whether the allow-list holds it.
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

    /// What an allow-list names it by: its kind, path and widget, without the line, so editing
    /// the file above it keeps it allowed.
    #[must_use]
    pub fn id(&self) -> String {
        format!("{} {} [{}]", self.kind.name(), self.path(), self.widget)
    }
}

/// What to check and how.
#[derive(Clone, Debug)]
pub struct Options {
    /// The app packages.
    pub dirs: Vec<PathBuf>,
    /// Where packages they use are found besides beside them.
    pub search: Vec<PathBuf>,
    /// The allow-list: findings it names are reported but do not fail the check.
    pub allow: Option<PathBuf>,
    /// Print the toolkit's inspection of every layout at this window size to stderr: each
    /// widget's box, clip and semantics, as the check reads them.
    pub dump: Option<(u32, u32)>,
    /// Write the window at the `dump` size, at display and text scale 1, as each stage leaves
    /// it, in the light and the dark theme, to PNGs in this directory.
    pub shots: Option<PathBuf>,
    /// The themes `shots` are taken in: a shipped theme by name (`glass`, `block`), in its
    /// light and dark variant, or a theme bundle's directory (`ui::theme_bundle`); none takes
    /// the stock light and dark.
    pub themes: Vec<String>,
    /// The layer of the package to lay out, when it has more than one root (`Desk`, the
    /// `shell` record): a shot of the shell's desk is `--root Desk`. None lays out the
    /// package's first root, as the launcher opens an app.
    pub root: Option<String>,
    /// View states every window is staged at, as the app's own code names them
    /// (`place=Task.today`, a filter of the type the state queries; `side=Side.right`, a case
    /// of its enum): the place a window stands on before anyone picks, for a shot of a state
    /// that is not the declaration's default.
    pub states: Vec<String>,
    /// Controls pressed by the words they carry, in order, before each window is read: what a
    /// person opens by pressing it (`--press "Since you looked"`, or the row of a task, which
    /// opens where it stands), where a click is the only way to reach it. A label under
    /// `keys`' rules: it reaches the window at the `dump` size.
    pub presses: Vec<String>,
    /// Key presses sent to each window before it is read, in order, as
    /// [`ui::headless::FrameHarness::key`] names them (`Tab`, `Down`, `Space`, or a chord that
    /// holds its modifiers for the press, `Shift+F10`): the layout a person reaches by
    /// keyboard, for its shot and its semantics. They reach the window at the `dump` size,
    /// where a shot shows them; none there, at the first layout.
    pub keys: Vec<String>,
    /// The language the person the windows are laid out for reads and types in, as a POSIX or
    /// BCP 47 tag (`en-US`, `de_DE.UTF-8`): what every window's language is chosen against,
    /// whatever locale the machine runs in. Every window of the run reads in one language
    /// throughout, its source language where the app ships no catalogue in this one.
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
            themes: Vec::new(),
            root: None,
            states: Vec::new(),
            presses: Vec::new(),
            keys: Vec::new(),
            locale: DEFAULT_LOCALE.to_owned(),
        }
    }
}

/// The language the check lays its windows out for unless `--locale` names another: the source
/// language of the toolkit and of the apps it ships, so a check says the same thing on every
/// machine, whatever locale the person running it has set.
pub const DEFAULT_LOCALE: &str = "en-US";

/// The light and the dark variant of the shipped theme `name`: `default` the stock pair,
/// another name its preset and that preset's light sibling (`glass`, `glass light`).
fn theme_pair(name: &str) -> Result<[(&'static str, ui::Theme); 2], String> {
    if ui::theme::Preset::find(name) == Some(ui::theme::Preset::Dark) || name == "default" {
        return Ok([("light", ui::Theme::light()), ("dark", ui::Theme::dark())]);
    }
    if ui::Theme::named(name).is_none() && Path::new(name).is_dir() {
        // A theme bundle: one document, shot as both.
        let dir = Path::new(name);
        let theme = load_theme_bundle(dir).map_err(|e| format!("--theme {name}: {e}"))?;
        return Ok([("light", theme.clone()), ("dark", theme)]);
    }
    let dark = ui::Theme::named(name).ok_or_else(|| format!("--theme {name}: no such theme"))?;
    let light = ui::Theme::named(&format!("{name} light")).unwrap_or_else(|| dark.clone());
    Ok([("light", light), ("dark", dark)])
}

/// The theme of the bundle directory `dir`: its [`ui::theme_bundle::DOCUMENT`] and a
/// `<set>.wasm` beside it for each set the document names. The toolkit reads no directory itself;
/// this is the dev tool's read of one.
fn load_theme_bundle(dir: &Path) -> Result<ui::Theme, ui::theme_bundle::BundleError> {
    let document = std::fs::read_to_string(dir.join(ui::theme_bundle::DOCUMENT))
        .map_err(|e| ui::theme_bundle::BundleError::Read(format!("{}: {e}", dir.display())))?;
    ui::theme_bundle::load(&document, |set| {
        let path = dir.join(format!("{set}.wasm"));
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(ui::theme_bundle::BundleError::Read(format!(
                "{}: {e}",
                path.display()
            ))),
        }
    })
}

/// The part of a shot's file name that says which theme family it is in:
/// the name, or a bundle directory's last component.
fn family_slug(family: &str) -> String {
    Path::new(family)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(family)
        .to_owned()
}

/// A view no layout showed a node of: the app and view (`app › view`), and whether any view of
/// any app called it.
#[derive(Debug, serde::Serialize)]
pub struct Unreached {
    /// `app › view`.
    pub view: String,
    /// Whether a view calls it, its own package's or one that uses it: a view nothing calls is
    /// one no window can show, and one something calls is a screen no scenario reached.
    pub called: bool,
}

impl std::fmt::Display for Unreached {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.view)
    }
}

/// What a check found.
#[derive(Debug, serde::Serialize)]
pub struct Report {
    pub findings: Vec<Finding>,
    /// Allow-list entries nothing showed for any more.
    pub stale: Vec<String>,
    /// Views no layout showed a node of (`app › view`), a list's rows included: either nothing
    /// calls them or no scenario reaches them.
    pub unreached: Vec<Unreached>,
    /// Layouts run, over every scenario of every app.
    pub layouts: usize,
    /// What laying every window out cost, in the toolkit's own units rather than in time: the
    /// frames laid out, the layout nodes built and the text shaped outside the cache. A budget
    /// test holds a layout to these, which no machine's load moves.
    pub work: Work,
    pub elapsed_ms: u64,
}

impl Report {
    /// Findings the allow-list does not hold.
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
///
/// The clock is the one a capture pins ([`ui::clock::freeze_for_capture`]): the app's queries
/// and the days its window writes read the same instant, and every run lays out the same
/// dates.
pub fn check(options: &Options) -> Result<Report, String> {
    let started = std::time::Instant::now();
    // Without the product's catalog this process's text falls back to the host's faces, so
    // every judgement about how it lays out would be about a typeface the product does not
    // ship. That is the environment, not the app: refuse rather than report on it.
    if let Some(problem) = ui::text_render::font_catalog_problem() {
        return Err(problem);
    }
    ui::clock::freeze_for_capture();
    stage_person_locale(&options.locale)?;
    tilo_ui_host::i18n::ensure();
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
    // Every scenario of every app, on every layer it has: the units of work, each a window and
    // a frame pipeline of its own, so they run at the same time.
    let scenes: Vec<Scene> = apps
        .iter()
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
        .collect();
    let mut planned = Vec::with_capacity(scenes.len().saturating_mul(3));
    for scene in &scenes {
        planned.extend(scene.units());
    }
    let mut phase = run(&apps, &planned, options)?;
    // The views nothing showed yet, opened on the scenario whose resources they take, with
    // what every scene before it showed: the scenes of one app decide in order, as the check
    // ran them one after another, while the apps decide at the same time.
    let (mut opened, shown) = run_opens(&apps, &scenes, &phase, options)?;
    // A dump is read as a page of one layout after another: the units ran at the same time, and
    // their pages are printed where the work stands, in the order the check would have done it
    // alone.
    for found in phase.iter().chain(opened.iter()) {
        eprint!("{}", found.dump);
    }
    let mut layouts_run: usize = 0;
    let mut work = Work::default();
    let mut gathered: Vec<BTreeMap<Identity, Finding>> = Vec::new();
    gathered.resize_with(apps.len(), BTreeMap::new);
    for ((scene, units), opened) in scenes
        .iter()
        .zip(phase.chunks_mut(3))
        .zip(opened.iter_mut())
    {
        let [scenario, press, push] = units else {
            continue;
        };
        layouts_run = layouts_run
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
    // A label no window carried anywhere is the one the run reports: a control a scenario
    // shows and the next does not — a window's change mark — is pressed where it stands,
    // rather than leaving every scenario without it unpressed.
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
        layouts: layouts_run,
        work,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

/// The views no layout showed a node of: a view its window draws off a call or from the
/// system's own surfaces (a row view, a proposal view) is not one.
///
/// A view drawn in any app's layout was laid out there, so what every app showed is what every
/// view is held to (Today draws Chat's Unread place). A view no call anywhere names is one no
/// window can show, and one a call names is a screen no scenario reached.
fn unreached_views(apps: &[App], shown: &[Shown]) -> Vec<Unreached> {
    let drawn: Shown = shown.iter().flatten().cloned().collect();
    let called: BTreeSet<(String, String)> = apps
        .iter()
        .flat_map(|held| held.sources.called())
        .map(|(app, view)| (app.to_owned(), view.to_owned()))
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

/// Stage the person the run lays its windows out for, in place of whatever locale this machine
/// runs in: every window's language is chosen from theirs, so the same sources lay out in the
/// same words on every machine ([`Options::locale`]).
fn stage_person_locale(locale: &str) -> Result<(), String> {
    let language = tilo_process::environment::parse_posix_locale(locale)
        .ok_or_else(|| format!("--locale {locale}: names no language (write `en-US`)"))?;
    let mut environment = tilo_process::environment::environment().as_ref().clone();
    environment.locale = Some(language.to_string());
    tilo_process::environment::install_environment(environment);
    Ok(())
}

/// The words the window showed from a catalogue in a language other than the one it speaks,
/// since the last frame was read: one problem per message, at the window. A window says
/// everything in one language (docs/designs/app-language/i18n.md §4), so a word from another
/// catalogue — a translation of another language, or the source standing in for one the
/// window's own catalogues lack — is what this holds it to.
fn mixed_language() -> Vec<audit::Problem> {
    let language = ui::i18n::I18nManager::window_locale();
    let found = ui::i18n::I18nManager::foreign_words();
    ui::i18n::I18nManager::clear_foreign_words();
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

/// One app of the check, read once: its metadata and its views' source, which every unit of its
/// work shares.
struct App {
    dir: PathBuf,
    short: String,
    meta: tilolang::PackageMeta,
    sources: source::SourceMap,
    /// The layers laid out: the window's own, and every other root the app declares.
    roots: Vec<Option<String>>,
    /// Its packages compiled, as a window loads them: a worker copies them into the window it
    /// lays out rather than compiling them again, which takes seconds to run.
    compiled: Vec<tilolang_tilo::tilolang::Compiled>,
    /// The packages it uses as metadata rather than as a package directory: the native apps'
    /// (each a committed description) and the platform's, which every window of it loads too.
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
    /// The units every scene has: the scenario as staged, its primary actions pressed, and what
    /// a row it opened shows a person holding the window.
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

    /// The views nothing showed yet, opened on the scenario whose resources they take, with
    /// what every scene before this one showed.
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

    /// Whether opening a view on this scenario may show one nothing showed yet: the views of
    /// types it did not open, and the views the system shows on surfaces of its own.
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
    }
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
    /// The scenario as staged, laid out every way.
    Scenario,
    /// The scenario with the views nothing showed yet opened on it, laid out every way.
    Opens { shown: Shown },
    /// The scenario with each of its primary actions pressed, at every size.
    Press,
    /// The scenario at every size it is held in the hand, with the row it opened tapped as a
    /// finger taps it: the window a person comes to after choosing what to read.
    Push,
}

/// What one unit of work found.
#[derive(Default)]
struct Found {
    gathered: BTreeMap<Identity, Finding>,
    /// Layouts run.
    layouts: usize,
    /// The views some layout showed a node of.
    shown: Shown,
    /// The `--press` labels a control carried in this unit of work.
    pressed: BTreeSet<String>,
    /// What laying its windows out cost, in the toolkit's own units.
    work: stage::Work,
    /// What the unit dumped (`--dump`): the toolkit's inspection of each layout at the dump
    /// size, held until every unit has run so the page reads in the order the work does.
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

/// The app in `dir` compiled after the packages it uses, found in `search`; and the packages
/// it uses as metadata rather than as a package directory: the native apps' (each a committed
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

/// The stack a worker lays a window out on: a window's tree is as deep as the documents it
/// shows, and a rich-text editor inside a row standing open needs more room than a thread's
/// default two megabytes, sized for a plain thread's work. A window in the app lays out on the
/// thread running the app, which is the main thread's; a worker gets the room to match.
pub(crate) const WORKER_STACK: usize = 32 * 1024 * 1024;

/// Start `worker` on a thread of the scope with a window's stack, and say why one could not
/// start: the work it would have taken then has nobody to run it.
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

/// Wait for any other layout check on this machine to finish, and hold the machine until this
/// one has: the lock lives as long as the returned guard.
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

/// Run every unit of `units` at the same time, each on a worker of its own, and return what each
/// found in the order given. A worker lays its windows out on packages of its own: the compiled
/// packages a window loads belong to one window at a time, so each worker copies the app's.
fn run(apps: &[App], units: &[Unit], options: &Options) -> Result<Vec<Found>, String> {
    if units.is_empty() {
        return Ok(Vec::new());
    }
    // Every worker below takes a core, so a second check at the same time only slows both.
    let _machine = machine_lock()?;
    let workers = std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(units.len());
    let taken: Vec<Mutex<Option<Result<Found, String>>>> =
        units.iter().map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let stopped = AtomicBool::new(false);
    let unstarted: Mutex<Option<String>> = Mutex::new(None);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let started = spawn_worker(scope, || {
                tilo_ui_host::i18n::ensure();
                let mut worker = Worker {
                    apps,
                    options,
                    text: ui::TextRenderer::new(),
                    loaded: None,
                };
                while !stopped.load(Ordering::Relaxed) {
                    let at = next.fetch_add(1, Ordering::Relaxed);
                    let Some(unit) = units.get(at) else {
                        break;
                    };
                    let found = worker.check(unit);
                    if found.is_err() {
                        stopped.store(true, Ordering::Relaxed);
                    }
                    if let Some(slot) = taken.get(at) {
                        *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(found);
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
    for slot in taken {
        let found = slot
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
            .ok_or("a unit of work did not run: another failed first")?;
        out.push(found?);
    }
    Ok(out)
}

/// What opening the views of one scene found, under the index of the scene it was staged on.
type Opened = Vec<(usize, Found)>;

/// Run `chain` for each app of `busy` at once, each on a worker of its own, and write what it found
/// in that app's slot. A worker that cannot start is reported, never left to leave its slot empty.
fn opens_together(
    busy: &[usize],
    slots: &[Mutex<Option<Result<Opened, String>>>],
    chain: &(impl Fn(usize) -> Result<Opened, String> + Sync),
) -> Result<(), String> {
    let unstarted: Mutex<Option<String>> = Mutex::new(None);
    std::thread::scope(|scope| {
        for &app in busy {
            let Some(slot) = slots.get(app) else {
                continue;
            };
            let started = spawn_worker(scope, move || {
                tilo_ui_host::i18n::ensure();
                *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(chain(app));
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

/// Run the views no layout showed yet, on the scene whose resources they take, for every app:
/// an app's scenes one after another, each seeing what the ones before it showed, and the apps
/// at the same time. Returns what each scene opened and the views every layout of each app
/// showed.
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
    let slots: Vec<Mutex<Option<Result<Opened, String>>>> =
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
            text: ui::TextRenderer::new(),
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
                let found = worker.check(&scene.opens(&seed))?;
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
    opens_together(&busy, &slots, &chain)?;
    let mut opened: Vec<Found> = (0..scenes.len()).map(|_| Found::default()).collect();
    let mut shown: Vec<Shown> = vec![Shown::new(); apps.len()];
    for (app, slot) in slots.into_iter().enumerate() {
        let Some(result) = slot.into_inner().unwrap_or_else(PoisonError::into_inner) else {
            continue;
        };
        let Some(held) = shown.get_mut(app) else {
            continue;
        };
        for (index, found) in result? {
            held.extend(found.shown.iter().cloned());
            if let Some(slot) = opened.get_mut(index) {
                *slot = found;
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
    for (slot, found) in held
        .iter_mut()
        .zip(phase.chunks(3).nth(index).unwrap_or_default())
    {
        *slot = Some(found);
    }
    held
}

/// One thread's check: the apps it reads, the packages of the app it is in, and the text it
/// measures with.
struct Worker<'a> {
    apps: &'a [App],
    options: &'a Options,
    text: ui::TextRenderer,
    /// The app whose packages are loaded, held beside them. A worker moves from one app to the
    /// next rarely (it takes the units in the order they come), so one is enough.
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
            themes: &self.options.themes,
            states: &self.options.states,
            keys: &self.options.keys,
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

    /// `app`'s packages as the window loads them, copied on the first unit of it this worker
    /// runs and again when the worker moves to another app.
    fn packages(&mut self, app: usize) -> Result<Rc<stage::Packages>, String> {
        if let Some((held, packages)) = &self.loaded
            && *held == app
        {
            return Ok(Rc::clone(packages));
        }
        let Some(held) = self.apps.get(app) else {
            return Err(format!("no app {app}"));
        };
        // A copy of what the app's own read compiled: the window loads packages of its own,
        // and compiling them again would spend seconds of every worker's first unit on it.
        let packages = Rc::new(stage::Packages(
            Rc::new(held.compiled.clone()),
            held.platform.clone(),
        ));
        self.loaded = Some((app, Rc::clone(&packages)));
        Ok(packages)
    }
}

/// The layers laid out. A package with more than one root declares layers the person sees
/// apart from one another — the shell's `Desk`, its `Arrangement` of places and its `Viewing`
/// of settings — and each has its own default view, so every one is laid out: the launcher
/// opens an app's first root, yet a person meets the others through the desktop's surfaces,
/// and a view only one of them reaches (a dock, a settings page) must still be held to the bar.
/// `--root NAME` narrows the check to that layer alone, as a shot of the desk is.
fn layers_of(meta: &tilolang::PackageMeta, root: Option<&str>) -> Vec<Option<String>> {
    if let Some(name) = root {
        return vec![Some(name.to_owned())];
    }
    let mut layers: Vec<Option<String>> = vec![None];
    // Every other root the package declares, by the name `--root` takes: a named root's own,
    // and the type's name for a root with none. The window's own root is laid out already,
    // whichever name it goes by.
    let others: BTreeSet<&str> = meta
        .named_roots
        .iter()
        .filter(|r| meta.roots.first() != Some(&r.ty))
        .map(|r| r.name.as_str())
        .collect();
    for ty in &meta.roots {
        let Some(declared) = meta.types.iter().find(|t| &t.id == ty) else {
            continue;
        };
        if meta.named_roots.iter().any(|r| &r.ty == ty) || meta.roots.first() == Some(ty) {
            // Already laid out: the first root is the window's own, and a named root has its
            // name below.
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
    rows: &std::collections::HashMap<String, tilo_ui_host::UiNode>,
    resources: &[ResourceRef],
) -> Vec<ResourceRef> {
    rows.keys()
        .filter_map(|key| {
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

/// The views of `meta` the system shows on surfaces of its own: each action's proposal view,
/// once each. A type's row view is not one: the system shows it as a row of a list (a search
/// result, a row of an agent's answer), so a page of it would be a surface nobody meets, and a
/// list of rows lays it out already.
fn surface_views(meta: &tilolang::PackageMeta) -> Vec<&tilolang::meta::ViewMeta> {
    let ids: BTreeSet<&str> = meta
        .row_views
        .iter()
        .map(|row| row.view.as_str())
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
/// window's bar offers as a switch between them (a project as a board beside its list). Only a
/// type that declares a default view and the views beside it (`views a, b for T`) has a
/// switch: every other view of a type is a helper its views place themselves.
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

/// The test id of the first control under `node` named `label` that a person can press: a
/// button, or a row of a list or a tree, which a press opens where it stands.
fn button_named(node: &ui::headless::InspectionNode, label: &str) -> Option<String> {
    let pressable = node.semantics.as_ref().is_some_and(|access| {
        matches!(
            access.role,
            ui::a11y::Role::Button
                | ui::a11y::Role::ToggleButton
                | ui::a11y::Role::ListItem
                | ui::a11y::Role::TreeItem
        ) && !access.state.disabled
            && access.name.as_deref() == Some(label)
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

/// How a press by its words reaches a window: the control of that name, or where a menu row of
/// that name stands — a row is painted by the menu that shows it, so only a press at where it
/// stands reaches it.
enum Pressing {
    /// A control, and whether a click reaches it rather than its semantic action.
    Control(String, bool),
    At(ui::kurbo::Rect),
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

/// The box of the row `label` among what `access` publishes, a menu's rows being its children's
/// children.
fn row_named(access: &ui::a11y::AccessNode, label: &str) -> Option<ui::kurbo::Rect> {
    for child in &access.children {
        if child.role == ui::a11y::Role::MenuItem
            && !child.state.disabled
            && child.name.as_deref() == Some(label)
            && let Some(bounds) = child.bounds
        {
            return Some(bounds.to_rect());
        }
        if let Some(bounds) = row_named(child, label) {
            return Some(bounds);
        }
    }
    None
}

/// The test id of the first widget under `node` that publishes a button named `label` as a
/// control it paints itself (an empty state's action): the widget is what is activated.
fn painted_button_named(node: &ui::headless::InspectionNode, label: &str) -> Option<String> {
    let painted = node.semantics.as_ref().is_some_and(|access| {
        access.children.iter().any(|child| {
            matches!(
                child.role,
                ui::a11y::Role::Button | ui::a11y::Role::ToggleButton
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

/// Whether `node` stands where a finger on the window reaches it: drawn, and at least partly
/// inside the window. What a push screen leaves behind it stands off the window, and a press
/// there is one nobody can make.
fn reachable(node: &ui::headless::InspectionNode, viewport: (u32, u32)) -> bool {
    node.rect[2] > 0.0
        && node.rect[3] > 0.0
        && node.rect[0] + node.rect[2] > 0.0
        && node.rect[1] + node.rect[3] > 0.0
        && node.rect[0] < f64::from(viewport.0)
        && node.rect[1] < f64::from(viewport.1)
}

/// Where a finger taps to open a row of a list: the middle of it, drawn where the person
/// holding the window can reach it.
fn row_centre(node: &ui::headless::InspectionNode) -> (f64, f64) {
    (
        node.rect[0] + node.rect[2] / 2.0,
        node.rect[1] + node.rect[3] / 2.0,
    )
}

/// Whether the window draws `node` as a row of a list: a list item, or a destination of the
/// tree a sidebar's list is.
fn is_row(node: &ui::headless::InspectionNode) -> bool {
    node.semantics.as_ref().is_some_and(|access| {
        matches!(
            access.role,
            ui::a11y::Role::ListItem | ui::a11y::Role::TreeItem
        ) && !access.state.disabled
    })
}

/// Where a finger taps to open what the window shows after its list: the middle of the row the
/// window selects for it, drawn where the person holding the window can reach it. A row a list
/// selects and the screen shows is the one the person whose window narrowed is looking at;
/// anything off the window (the places behind the list) is not.
fn row_opened(node: &ui::headless::InspectionNode, viewport: (u32, u32)) -> Option<(f64, f64)> {
    let selected = node
        .semantics
        .as_ref()
        .is_some_and(|access| access.state.selected);
    if selected && is_row(node) && reachable(node, viewport) {
        return Some(row_centre(node));
    }
    node.children
        .iter()
        .find_map(|child| row_opened(child, viewport))
}

/// Where a finger taps to open a row when the window keeps none chosen: the middle of the first
/// row of its list, drawn where the person holding the window can reach it. A list that keeps
/// no row of its own — the places, which the app keeps — is one a person still picks a row of.
fn first_row(node: &ui::headless::InspectionNode, viewport: (u32, u32)) -> Option<(f64, f64)> {
    if is_row(node) && reachable(node, viewport) {
        return Some(row_centre(node));
    }
    node.children
        .iter()
        .find_map(|child| first_row(child, viewport))
}

/// Whether the window shows its way back where a finger reaches it: how a person holding a
/// narrow window comes back to the list it opened something from. The desktop's bar over the
/// window carries it, as its own navigation or the shell's slot; an app in a window of its own
/// draws a chip in its frame.
fn back_chip(staged: &mut stage::Staged, viewport: (u32, u32)) -> bool {
    fn in_frame(node: &ui::headless::InspectionNode, viewport: (u32, u32)) -> bool {
        (reachable(node, viewport) && node.test_id.as_deref() == Some(ui::widget::WINDOW_BACK))
            || node.children.iter().any(|child| in_frame(child, viewport))
    }
    if in_frame(&staged.harness().inspection().root, viewport) {
        return true;
    }
    let bar = staged
        .harness_mut()
        .window_bar_layout(f64::from(viewport.0));
    bar.controls.iter().any(|control| {
        (control.slot == "back" || control.id == ui::widget::WINDOW_BACK)
            && control.rect[2] > 0.0
            && control.rect[3] > 0.0
    })
}

/// Where a finger taps to open a row of the list the window shows now: the row it keeps
/// chosen, or else the first of the list.
fn tappable_row(staged: &stage::Staged, viewport: (u32, u32)) -> Option<(f64, f64)> {
    let root = &staged.harness().inspection().root;
    row_opened(root, viewport).or_else(|| first_row(root, viewport))
}

/// One app's check under way.
struct AppCheck<'a> {
    dir: &'a Path,
    short: &'a str,
    meta: &'a tilolang::PackageMeta,
    sources: &'a source::SourceMap,
    dump: Option<(u32, u32)>,
    shots: Option<&'a Path>,
    themes: &'a [String],
    states: &'a [String],
    keys: &'a [String],
    presses: &'a [String],
    gathered: BTreeMap<Identity, Finding>,
    /// Measures text as the window's pipeline does, for clipped text.
    text: &'a mut ui::TextRenderer,
    ran: usize,
    /// The views some layout showed a node of.
    shown: Shown,
    /// The `--press` labels a control carried, so a label no layout carries at all is the one
    /// the run reports: a mark a window shows in one scenario and not another is pressed where
    /// it stands.
    pressed: BTreeSet<String>,
    /// What laying this unit's windows out cost, in the toolkit's own units.
    work: stage::Work,
    /// The `--dump` page of this unit of work, as [`Found::dump`].
    dumped: String,
}

impl AppCheck<'_> {
    /// One unit of the app's work, from the scenario as staged.
    fn unit(&mut self, packages: &Rc<stage::Packages>, unit: &Unit) -> Result<(), String> {
        let title = on_layer(&unit.scene.scenario, unit.scene.root.as_deref());
        match unit.what {
            What::Scenario => {
                let mut staged = self.stage(packages, unit)?;
                self.run(&mut staged, &title)
            }
            What::Opens { .. } => {
                let mut staged = self.stage(packages, unit)?;
                self.open_views(&mut staged, &title)
            }
            What::Press => self.press(packages, unit),
            What::Push => self.push(packages, unit),
        }
    }

    /// The scenario as staged, laid out once at the smallest window so a window's first frame
    /// is drawn before its layouts are read.
    fn stage(&self, packages: &Rc<stage::Packages>, unit: &Unit) -> Result<stage::Staged, String> {
        let first = layouts().first().copied().ok_or("no layouts")?;
        stage::Staged::over(
            &self.meta.app,
            packages,
            &unit.scene.scenario,
            (first.size.width, first.size.height),
            ui::Theme::light(),
            unit.scene.root.as_deref(),
            self.states,
        )
    }

    /// The views of types the scenario did not open, on a resource its window names; and the
    /// views the system shows on surfaces of its own rather than in a window of the app: a
    /// type's row view (a search result, a row of an agent's answer) and an action's proposal
    /// view (the card asking to run it), on a resource of the type they take. A view another
    /// layout showed already is not opened again.
    fn open_views(&mut self, staged: &mut stage::Staged, title: &str) -> Result<(), String> {
        // A page a person meets from a list is one of that list's own rows, so a view staged over
        // the list opens a resource the list holds where it holds one, and a resource of a type
        // the list does not show where it holds none.
        let rows = staged.rows();
        let named = staged.resources();
        let listed = row_items(&rows, &named);
        for default in &self.meta.default_views {
            let Some(view) = self.meta.views.iter().find(|v| v.id == default.view) else {
                continue;
            };
            // The root's own view is the window's first column already.
            if shows(&self.shown, &self.meta.app, &view.name)
                || self.meta.roots.contains(&default.ty)
            {
                continue;
            }
            let Some(resource) = resource_of(&listed, &named, &default.ty) else {
                continue;
            };
            staged.open(resource.clone());
            self.run(staged, &format!("{title}, then a {} opened", view.name))?;
            // An app whose rows open where they stand (a task opened in its list) answers the
            // record's own door with its place: its page is still a view the app draws, from
            // the row's menu, so the run opens it by its name as that menu row does.
            if !shows(&self.shown, &self.meta.app, &view.name) {
                staged.open_with(&resource, &view.id);
                self.run(
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
            let Some(resource) = resource_of(&listed, &named, &ty.id) else {
                continue;
            };
            staged.open_with(&resource, &view.id);
            self.run(staged, &format!("{title}, then {} shown", view.name))?;
        }
        // The views a type's bar switches to beside the one it opens on, on a resource of the
        // type: a project laid out as a board as well as the list it opens on.
        let meta = self.meta;
        for (view, ty) in switch_views(meta) {
            if shows(&self.shown, &self.meta.app, &view.name) {
                continue;
            }
            let Some(resource) = resource_of(&listed, &named, &ty.id) else {
                continue;
            };
            staged.open_with(&resource, &view.id);
            self.run(staged, &format!("{title}, then {} shown", view.name))?;
        }
        Ok(())
    }

    /// What the window shows once a person pressed one of its primary actions (a new note
    /// opened beside the list, its header row laid out again around it): at each size, from the
    /// scenario as staged, pressed as a person presses (a tap on a touchscreen, a click
    /// otherwise) and with the motion it starts run out, read as it stands then, before anything
    /// lays the window out afresh. A layout that only a fresh pass puts right (a header row left
    /// where a frame of the motion put it) shows here.
    fn press(&mut self, packages: &Rc<stage::Packages>, unit: &Unit) -> Result<(), String> {
        for label in primary_labels(self.meta) {
            for size in SIZES {
                let layout = Layout {
                    size,
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
    /// What a person holding the window comes to after choosing what to read: at every size it
    /// is held in the hand, a row of the list the window shows tapped as a finger taps it,
    /// which pushes what the row opens in front of that list, and the list itself brought back
    /// by its Back chip where the state the app opened stands in front of it. A scene whose
    /// window opened nothing has no such state, and no list to come back to.
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
            // What the app opened stands in front of the list it came from: a scenario that
            // returns a resource shows that resource's page, and a row of the page is the
            // page's own, where tapping it chooses what it shows. A finger goes back to the
            // list first, and taps a row of that.
            let mut page_in_front = self.scenario_returns_a_resource(&unit.scene.scenario);
            let mut tapped = if page_in_front {
                None
            } else {
                tappable_row(&staged, viewport)
            };
            if tapped.is_none() {
                if !back_chip(&mut staged, viewport) {
                    continue;
                }
                let shown = staged.front_target();
                staged.press_bar_slot(ui::widget::WINDOW_BACK)?;
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
            // The row the window keeps chosen, or else the first of the list: what a person
            // reaches for, which opens in front of the list as the app declares.
            let Some(tapped) = tapped else {
                continue;
            };
            let before = staged.front_target();
            staged.tap_at(tapped)?;
            // The tap shows what its row opens where the list was; a row whose tap opens
            // nothing leaves the list showing, and the shot's name says so.
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

    /// Whether the scenario the window was staged from returned a resource of its own
    /// (`scenario "a busy week" -> Task`): the window shows that resource's page in front of
    /// the list the scenario made.
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

    /// The view a window opens `target` in: its type's default view, which is what a row of it
    /// opens.
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
    fn run(&mut self, staged: &mut stage::Staged, label: &str) -> Result<(), String> {
        // The keys reach the window at the size it is read at, where a shot shows them; a
        // check taken without a dump size reads them at its first layout. Pressed once: at
        // every layout the presses would pile up, folding and opening what the one before had
        // left.
        let read_at = self.dump.or_else(|| {
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
                && (!self.keys.is_empty() || !self.presses.is_empty())
            {
                // What the person sees before they touch it, where the run takes shots: a
                // press leaves the window as it opened something, and both are what a person
                // reads.
                if let Some(dir) = self.shots
                    && self.dump == Some(viewport)
                    && (layout.display_scale - 1.0).abs() < f64::EPSILON
                    && (layout.text_scale - 1.0).abs() < f32::EPSILON
                {
                    self.shoot(staged, layout, label, dir)?;
                }
                // What a person opens first, then what they reach by keyboard: the window is
                // laid out as they leave it, with a popover hanging and a control wearing its
                // ring. A label no window of the app carries is pressed in the windows that
                // do carry it ([`Found::pressed`]).
                for press in self.presses {
                    let found = {
                        let root = &staged.harness().inspection().root;
                        button_named(root, press)
                            .map(|id| Pressing::Control(id, true))
                            .or_else(|| {
                                painted_button_named(root, press)
                                    .map(|id| Pressing::Control(id, false))
                            })
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
                        Pressing::At(bounds) => {
                            let center = bounds.center();
                            staged.press_at(center.x, center.y)?;
                        }
                    }
                    // What the press opened — a menu over the window, a popover beside a
                    // control — is laid out before the next word is looked for.
                    staged.freshen()?;
                }
                for key in self.keys {
                    staged.key(key);
                }
                staged.lay_out(
                    viewport,
                    layout.theme(),
                    layout.input(),
                    layout.display_scale,
                )?;
                pressed = true;
                // The shot and the findings as the press leaves the window, under a label of
                // their own: the window it opened is never the one the scenario staged.
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

    /// Gather what is wrong with `staged` as it is laid out now, at `layout`, as staged by
    /// `label`.
    fn audit(
        &mut self,
        staged: &mut stage::Staged,
        layout: Layout,
        label: &str,
    ) -> Result<(), String> {
        let theme = layout.theme();
        self.text.set_text_scale(theme.text_scale);
        self.text.apply_typography(&theme.tokens.typography);
        let viewport = (layout.size.width, layout.size.height);
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
        self.ran = self.ran.saturating_add(1);
        let inspection = staged.harness().inspection();
        // The bar the desktop draws over the frame: the app's own slots merged with the window's
        // navigation, its one "…" and its controls.
        let bar = staged
            .harness_mut()
            .window_bar_layout(f64::from(viewport.0));
        if self.dump == Some(viewport) {
            let json = serde_json::to_string_pretty(&bar).map_err(|e| e.to_string())?;
            let _ = writeln!(
                self.dumped,
                "== {} {label} at {layout} — the bar\n{json}",
                self.meta.app
            );
            let json = serde_json::to_string_pretty(&inspection).map_err(|e| e.to_string())?;
            let _ = writeln!(
                self.dumped,
                "== {} {label} at {layout} — the frame\n{json}",
                self.meta.app
            );
            if let Some(dir) = self.shots
                && (layout.display_scale - 1.0).abs() < f64::EPSILON
                && (layout.text_scale - 1.0).abs() < f32::EPSILON
            {
                self.shoot(staged, layout, label, dir)?;
            }
        }
        self.note_shown(&inspection.root);
        // A view that ran but drew nothing has no widget; what the app sent still names it. What
        // it declared its controls do is read off the same trees.
        let mut declared = ux::Declared::default();
        if let Some(composed) = staged.composed() {
            self.note_shown_row(&composed);
            declared.read(&composed);
        }
        let rows = staged.rows();
        for row in rows.values() {
            self.note_shown_row(row);
            declared.read(row);
        }
        let mut problems = audit::inspect(
            &inspection.root,
            viewport,
            layout
                .size
                .touch
                .then(|| f64::from(layout.input().minimum_target())),
            layout.display_scale,
        );
        problems.extend(audit::clipped_text(
            staged.harness_mut().root_widget_mut(),
            self.text,
            viewport,
        ));
        problems.extend(ux::inspect(
            &inspection.root,
            viewport,
            &declared,
            ui::chrome::disclosure_gutter(&layout.theme()),
        ));
        problems.extend(mixed_language());
        problems.extend(audit::bar(&bar));
        // A window shows one bar: a header row under it is a second one.
        let (bar_height, _) = staged.harness().frame_extent(viewport);
        problems.extend(audit::stacked_header_rows(
            bar_height,
            staged.harness().window_header_row_height(),
        ));
        problems.extend(audit::bar_primary(
            ui::window_toolbar::shown().as_ref(),
            &bar,
        ));
        for mut problem in problems {
            // A list draws a row as one widget keyed by the row: what it is about is the row
            // view's node, the deepest the row's tree holds.
            if let Some(row) = problem.id.as_ref().and_then(|id| rows.get(id)) {
                let mut inner = Vec::new();
                row_keys(row, &mut inner);
                inner.reverse();
                inner.append(&mut problem.keys);
                problem.keys = inner;
                problem.id = None;
            }
            self.gather(&problem, layout, label);
        }
        self.work.add(staged.take_work());
        Ok(())
    }

    /// Write the window as `label` leaves it at `layout` to `dir`, light and dark, and lay it
    /// out again as it was.
    fn shoot(
        &self,
        staged: &mut stage::Staged,
        layout: Layout,
        label: &str,
        dir: &Path,
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
                staged.lay_out(viewport, theme, layout.input(), 1.0)?;
                let file = if family.is_empty() {
                    dir.join(format!("{}-{slug}-{name}.png", self.short))
                } else {
                    dir.join(format!(
                        "{}-{slug}-{}-{name}.png",
                        self.short,
                        family_slug(family)
                    ))
                };
                staged.harness_mut().render_window_png(&file);
            }
        }
        staged.lay_out(viewport, layout.theme(), layout.input(), 1.0)
    }

    /// Remember the views shown in `key`: the view the node is of, and every view whose call
    /// holds it — of this app or of any other whose views this window draws.
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
    fn note_shown_row(&mut self, node: &tilo_ui_host::UiNode) {
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

/// The keys of a row's tree, outermost first, leaving out its slots (a menu, a swipe).
fn row_keys(node: &tilo_ui_host::UiNode, out: &mut Vec<String>) {
    if node.kind == "slot" {
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
    out
}

#[cfg(test)]
mod windows;

#[cfg(test)]
mod board_lane {
    use std::path::Path;
    use std::rc::Rc;

    use super::stage;
    use ui::kurbo::Rect;
    use ui::widget::Widget;

    const BOARD: &str = "so.tilo.lang.layoutboard";

    fn packages() -> Rc<stage::Packages> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/layout_check/board");
        let search = vec![dir.parent().expect("tests/layout_check").to_path_buf()];
        let (built, platform) = super::build(&dir, &search).expect("the board fixture compiles");
        let app = built.last().expect("the fixture").compiled.meta.app.clone();
        assert_eq!(app, BOARD, "the fixture's app id");
        Rc::new(stage::Packages(
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

    /// Whether `rect` stands whole inside `clip`: a card cut by the view that scrolls its lane
    /// is not one a person sees.
    fn inside(clip: Rect, rect: Rect) -> bool {
        /// `layout` places edges on fractions of a pixel.
        const SLACK: f64 = 1.0;
        rect.height() > SLACK
            && rect.x0 >= clip.x0 - SLACK
            && rect.y0 >= clip.y0 - SLACK
            && rect.x1 <= clip.x1 + SLACK
            && rect.y1 <= clip.y1 + SLACK
    }

    /// Whether `node` is a card as a board draws one: the card's own chrome around a body, or
    /// the row a card component showed as its surface ([`ui::widget::CardBody`]), which a board's
    /// card is.
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

    /// A board's page stands the window's height, so its lanes are as tall as the window: a lane
    /// holding as many cards as the window shows draws every one of them under its heading, and
    /// a lane holding more keeps them inside the view that scrolls it. A page that grows with
    /// its lanes instead pushes the board past the window, and its first lane cuts the cards the
    /// window had room for.
    #[test]
    fn a_lane_draws_every_card_the_window_has_room_for() {
        tilo_ui_host::i18n::ensure();
        let packages = packages();
        let staged = stage::Staged::over(
            BOARD,
            &packages,
            "a board",
            (1280, 800),
            ui::Theme::light(),
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
        // Lanes stand side by side: the leftmost holds six cards, which the window shows at
        // once, and the rightmost thirty, more than its lane has room for.
        let left = whole
            .iter()
            .map(|rect| rect.x0)
            .fold(f64::INFINITY, f64::min);
        let right = whole
            .iter()
            .map(|rect| rect.x0)
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(right > left + 1.0, "the board shows more than one lane");
        let lane = |x: f64| {
            whole
                .iter()
                .filter(|rect| (rect.x0 - x).abs() < 1.0)
                .count()
        };
        assert_eq!(
            lane(left),
            6,
            "the first lane draws all six cards the window has room for"
        );
        assert!(
            lane(right) < 30,
            "the third lane scrolls its thirty cards instead of growing past the window"
        );
    }

    /// The board's sideways scroller under `root`: the lane strip it scrolls, the part of that
    /// strip the pane shows, and the lanes on the strip in lane order.
    fn across(root: &dyn Widget) -> (&ui::widget::Scrollable, Rect, Rect, Vec<Rect>) {
        let mut scrolls = Vec::new();
        named(root, "Scrollable", &mut scrolls);
        let pane = scrolls
            .into_iter()
            .find_map(|scroll| {
                let scroll = scroll.as_any()?.downcast_ref::<ui::widget::Scrollable>()?;
                matches!(
                    scroll.layout_info(),
                    Some(ui::widget::LayoutInfo::Scroll(ui::widget::ScrollLayout {
                        direction: ui::widget::ScrollDirection::Horizontal,
                    }))
                )
                .then_some(scroll)
            })
            .expect("the board scrolls across");
        let viewed = pane.child_clip_rect().unwrap_or_else(|| pane.get_rect());
        let strip = pane
            .children()
            .first()
            .map(|content| content.get_rect())
            .expect("the board's lanes stand on a strip");
        let lanes = pane
            .children()
            .iter()
            .flat_map(|content| content.children())
            .map(Widget::get_rect)
            .collect();
        (pane, viewed, strip, lanes)
    }

    /// The width a lane reads at, which every board gives it (`metrics.board_lane_width`).
    fn lane_measure() -> f64 {
        f64::from(ui::Theme::light().tokens.metrics.board_lane_width)
    }

    /// A pane narrower than a board's lanes scrolls them sideways instead of cutting them off:
    /// each lane keeps its measure and stands whole on the strip, the strip keeps the same
    /// gutter at each end, every lane the viewport shows is whole from its heading to its foot,
    /// and the last lane comes whole into view at the end of the scroll.
    #[test]
    fn a_narrow_board_scrolls_its_lanes_whole_across_the_pane() {
        tilo_ui_host::i18n::ensure();
        let packages = packages();
        let staged = stage::Staged::over(
            BOARD,
            &packages,
            "a board",
            (864, 800),
            ui::Theme::light(),
            None,
            &[],
        )
        .expect("the board stages");
        let root = staged.harness().root_widget();
        let (pane, viewed, strip, lanes) = across(root);
        assert_eq!(lanes.len(), 3, "three lanes: {lanes:?}");
        assert!(
            pane.horizontal_overflows(),
            "three lanes of {measure} outrun the 864 pane, so it scrolls them: {strip:?} in {viewed:?}",
            measure = lane_measure()
        );
        for lane in &lanes {
            assert!(
                (lane.width() - lane_measure()).abs() < 1.0,
                "a lane keeps its measure: {lane:?} at {measure}",
                measure = lane_measure()
            );
            assert!(
                inside(strip, *lane),
                "a lane stands whole on the strip: {lane:?} in {strip:?}"
            );
        }
        let start = lanes[0].x0 - strip.x0;
        let end = strip.x1 - lanes[2].x1;
        assert!(
            start > 1.0,
            "the first lane keeps clear of the strip's edge: {strip:?}"
        );
        assert!(
            (start - end).abs() < 1.0,
            "the lanes keep the same gutter at both ends of the strip: {start} before, {end} after"
        );
        // The strip is the pane's whole width and height: it stands inside the page it is
        // shown in, so nothing but the lanes' scroll moves it.
        let mut pages = Vec::new();
        named(root, "Page", &mut pages);
        assert_eq!(pages.len(), 1, "the board is one page");
        assert!(
            inside(pages[0].get_rect(), pane.get_rect()),
            "the view that scrolls the board stands inside the page: {:?} in {:?}",
            pane.get_rect(),
            pages[0].get_rect()
        );
        // Every lane the pane shows now is whole from its heading to its foot: only the lanes
        // scrolled out of view are out of reach until the person scrolls to them.
        let shown: Vec<Rect> = lanes
            .iter()
            .copied()
            .filter(|lane| lane.x1 > viewed.x0 && lane.x0 < viewed.x1)
            .collect();
        assert!(!shown.is_empty(), "the pane shows lanes: {viewed:?}");
        for lane in &shown {
            assert!(
                lane.height() > 1.0 && lane.y0 >= viewed.y0 - 1.0 && lane.y1 <= viewed.y1 + 1.0,
                "a lane the pane shows is whole, heading to foot: {lane:?} in {viewed:?}"
            );
        }
        // The end of the scroll brings the last lane whole into view, less the gutter after it.
        let shift = strip.width() - viewed.width();
        let reached = Rect::new(viewed.x0 + shift, viewed.y0, viewed.x1 + shift, viewed.y1);
        assert!(
            inside(reached, lanes[2]),
            "the last lane comes whole into view at the end of the scroll: {:?} in {reached:?}",
            lanes[2]
        );
    }

    /// A pane wide enough for a board's lanes shows them all at rest and offers no sideways
    /// scroll: each keeps its measure, the strip ends inside the pane, and the room the wider
    /// pane leaves open stays beside the lanes rather than stretching them.
    #[test]
    fn a_board_with_room_for_its_lanes_offers_no_sideways_scroll() {
        tilo_ui_host::i18n::ensure();
        let packages = packages();
        let staged = stage::Staged::over(
            BOARD,
            &packages,
            "a board",
            (1280, 800),
            ui::Theme::light(),
            None,
            &[],
        )
        .expect("the board stages");
        let root = staged.harness().root_widget();
        let (pane, viewed, strip, lanes) = across(root);
        assert_eq!(lanes.len(), 3, "three lanes: {lanes:?}");
        assert!(
            !pane.horizontal_overflows(),
            "the lanes fit the pane, so it scrolls across nothing: {strip:?} in {viewed:?}"
        );
        assert!(
            strip.x1 <= viewed.x1 + 1.0,
            "the strip ends inside the pane: {strip:?} in {viewed:?}"
        );
        for lane in &lanes {
            assert!(
                (lane.width() - lane_measure()).abs() < 1.0,
                "a lane keeps its measure where the pane has room to spare: {lane:?}"
            );
            assert!(
                inside(viewed, *lane),
                "every lane stands whole in a pane wide enough for it: {lane:?} in {viewed:?}"
            );
        }
    }
}
