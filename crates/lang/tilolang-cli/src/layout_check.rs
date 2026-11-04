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
mod stage;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub use audit::Kind;
use ui::interaction::InteractionEnvironment;

/// A window size people use, in logical pixels, and whether it is held in the hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub width: u32,
    pub height: u32,
    pub touch: bool,
}

/// Phones, a tablet, a laptop, a desktop monitor and an ultrawide one.
pub const SIZES: [Size; 6] = [
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
#[derive(Clone, Debug, Default)]
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
    /// Key presses sent to each window before it is read, in order, as
    /// [`ui::headless::FrameHarness::key`] names them (`Tab`, `Down`, `Space`, or a chord that
    /// holds its modifiers for the press, `Shift+F10`): the layout a person reaches by
    /// keyboard, for its shot and its semantics. They reach the window at the `dump` size,
    /// where a shot shows them; none there, at the first layout.
    pub keys: Vec<String>,
}

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

/// What a check found.
#[derive(Debug, serde::Serialize)]
pub struct Report {
    pub findings: Vec<Finding>,
    /// Allow-list entries nothing showed for any more.
    pub stale: Vec<String>,
    /// Views no layout showed a node of (`app › view`), a list's rows included: no scenario
    /// reaches them.
    pub unreached: Vec<String>,
    /// Layouts run, over every scenario of every app.
    pub layouts: usize,
    pub elapsed_ms: u64,
}

impl Report {
    /// Findings the allow-list does not hold.
    pub fn new_findings(&self) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(|f| !f.allowed)
    }
}

/// The ids an allow-list file names: one per line, `#` starting a comment.
///
/// # Errors
/// The file cannot be read.
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
///
/// # Errors
/// An app does not compile, or its window refuses what it sends.
pub fn check(options: &Options) -> Result<Report, String> {
    let started = std::time::Instant::now();
    ui::clock::freeze_for_capture();
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
    let mut planned = Vec::with_capacity(scenes.len().saturating_mul(2));
    for scene in &scenes {
        planned.extend(scene.units());
    }
    let mut phase = run(&apps, &planned, options)?;
    // The views nothing showed yet, opened on the scenario whose resources they take, with
    // what every scene before it showed: the scenes of one app decide in order, as the check
    // ran them one after another, while the apps decide at the same time.
    let (mut opened, shown) = run_opens(&apps, &scenes, &phase, options)?;
    let mut layouts_run: usize = 0;
    let mut gathered: Vec<BTreeMap<Identity, Finding>> = Vec::new();
    gathered.resize_with(apps.len(), BTreeMap::new);
    for ((scene, pair), opened) in scenes
        .iter()
        .zip(phase.chunks_mut(2))
        .zip(opened.iter_mut())
    {
        let [scenario, press] = pair else {
            continue;
        };
        layouts_run = layouts_run
            .saturating_add(scenario.layouts)
            .saturating_add(press.layouts)
            .saturating_add(opened.layouts);
        let Some(held) = gathered.get_mut(scene.app) else {
            continue;
        };
        merge(held, std::mem::take(&mut scenario.gathered));
        merge(held, std::mem::take(&mut opened.gathered));
        merge(held, std::mem::take(&mut press.gathered));
    }
    let mut findings: Vec<Finding> = gathered
        .into_iter()
        .flat_map(BTreeMap::into_values)
        .collect();
    let mut unreached = Vec::new();
    for (held, shown) in apps.iter().zip(&shown) {
        for view in &held.meta.views {
            if view.presents.is_none() && view.row_of.is_none() && !shown.contains(&view.name) {
                unreached.push(format!("{} › {}", held.short, view.name));
            }
        }
    }
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
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

/// One app of the check, read once: its metadata and its views' source, which every unit of its
/// work shares.
struct App {
    dir: PathBuf,
    /// Where its package and the ones it uses are found.
    search: Vec<PathBuf>,
    short: String,
    meta: tilolang::PackageMeta,
    sources: source::SourceMap,
    /// The layers laid out: the window's own, and every other root the app declares.
    roots: Vec<Option<String>>,
}

impl App {
    fn read(dir: &Path, search: &[PathBuf], root: Option<&str>) -> Result<Self, String> {
        let mut search = search.to_vec();
        search.extend(dir.parent().map(Path::to_path_buf));
        let (built, _platform) = build(dir, &search)?;
        let sources = source::SourceMap::new(&built);
        let meta = built.last().ok_or("no package")?.compiled.meta.clone();
        let short = meta.app.rsplit('.').next().unwrap_or(&meta.app).to_owned();
        Ok(Self {
            dir: dir.to_path_buf(),
            search,
            short,
            roots: layers_of(&meta, root),
            meta,
            sources,
        })
    }
}

/// The identity of a finding while it is gathered: kind, app, view, node, widget.
type Identity = (Kind, String, String, Option<String>, String);

/// One scenario of one app on one layer, the window laid out every way people lay one out.
struct Scene {
    app: usize,
    root: Option<String>,
    scenario: String,
}

impl Scene {
    /// The units every scene has: the scenario as staged, and its primary actions pressed.
    fn units(&self) -> [Unit; 2] {
        [
            Unit {
                scene: self.at(),
                what: What::Scenario,
            },
            Unit {
                scene: self.at(),
                what: What::Press,
            },
        ]
    }

    /// The views nothing showed yet, opened on the scenario whose resources they take, with
    /// what every scene before this one showed.
    fn opens(&self, shown: &BTreeSet<String>) -> Unit {
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
    fn needs_opens(&self, apps: &[App], shown: &BTreeSet<String>) -> bool {
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
                    .is_some_and(|v| !shown.contains(&v.name))
        });
        default
            || switch_views(meta)
                .iter()
                .any(|(view, _)| !shown.contains(&view.name))
            || surface_views(meta).iter().any(|view| {
                !shown.contains(&view.name)
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
    Opens { shown: BTreeSet<String> },
    /// The scenario with each of its primary actions pressed, at every size.
    Press,
}

/// What one unit of work found.
#[derive(Default)]
struct Found {
    gathered: BTreeMap<Identity, Finding>,
    /// Layouts run.
    layouts: usize,
    /// The views some layout showed a node of.
    shown: BTreeSet<String>,
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

/// Run every unit of `units` at the same time, each on a worker of its own, and return what each
/// found in the order given. A worker reads an app's package itself: the compiled packages a
/// window loads belong to one window at a time, so no two workers share them.
fn run(apps: &[App], units: &[Unit], options: &Options) -> Result<Vec<Found>, String> {
    if units.is_empty() {
        return Ok(Vec::new());
    }
    // A dump is read as a page of one layout after another, so it runs alone.
    let workers = if options.dump.is_some() {
        1
    } else {
        std::thread::available_parallelism()
            .map_or(1, std::num::NonZero::get)
            .min(units.len())
    };
    let taken: Vec<Mutex<Option<Result<Found, String>>>> =
        units.iter().map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let stopped = AtomicBool::new(false);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
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
        }
    });
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

/// Run the views no layout showed yet, on the scene whose resources they take, for every app:
/// an app's scenes one after another, each seeing what the ones before it showed, and the apps
/// at the same time. Returns what each scene opened and the views every layout of each app
/// showed.
fn run_opens(
    apps: &[App],
    scenes: &[Scene],
    phase: &[Found],
    options: &Options,
) -> Result<(Vec<Found>, Vec<BTreeSet<String>>), String> {
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
            let [scenario, press] = scene_units(phase, index);
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
            opened.push((index, found));
        }
        Ok(opened)
    };
    if options.dump.is_some() {
        for app in busy {
            if let Some(slot) = slots.get(app) {
                *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(chain(app));
            }
        }
    } else {
        std::thread::scope(|scope| {
            for app in busy {
                let Some(slot) = slots.get(app) else {
                    continue;
                };
                scope.spawn(move || {
                    tilo_ui_host::i18n::ensure();
                    *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(chain(app));
                });
            }
        });
    }
    let mut opened: Vec<Found> = (0..scenes.len()).map(|_| Found::default()).collect();
    let mut shown: Vec<BTreeSet<String>> = vec![BTreeSet::new(); apps.len()];
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
        let [scenario, press] = scene_units(phase, index);
        let Some(held) = shown.get_mut(scene.app) else {
            continue;
        };
        if let Some(scenario) = scenario {
            held.extend(scenario.shown.iter().cloned());
        }
        if let Some(press) = press {
            held.extend(press.shown.iter().cloned());
        }
    }
    Ok((opened, shown))
}

/// The two units every scene has in the first phase, in the order the check ran them.
fn scene_units(phase: &[Found], index: usize) -> [Option<&Found>; 2] {
    let mut held = [None, None];
    for (slot, found) in held
        .iter_mut()
        .zip(phase.chunks(2).nth(index).unwrap_or_default())
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
            keys: &self.options.keys,
            gathered: BTreeMap::new(),
            text: &mut self.text,
            ran: 0,
            shown: match &unit.what {
                What::Opens { shown } => shown.clone(),
                What::Scenario | What::Press => BTreeSet::new(),
            },
        };
        check.unit(&packages, unit)?;
        Ok(Found {
            gathered: check.gathered,
            layouts: check.ran,
            shown: check.shown,
        })
    }

    /// `app`'s package compiled, on the first unit of it this worker runs, and again when the
    /// worker moves to another app.
    fn packages(&mut self, app: usize) -> Result<Rc<stage::Packages>, String> {
        if let Some((held, packages)) = &self.loaded
            && *held == app
        {
            return Ok(Rc::clone(packages));
        }
        let Some(held) = self.apps.get(app) else {
            return Err(format!("no app {app}"));
        };
        let (built, platform) = build(&held.dir, &held.search)?;
        let packages = Rc::new(stage::Packages(
            Rc::new(built.into_iter().map(|b| b.compiled).collect()),
            platform,
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

/// The views of `meta` the system shows on surfaces of its own: each type's row view and each
/// action's proposal view, once each.
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

/// The test id of the first button under `node` named `label` that a person can press.
fn button_named(node: &ui::headless::InspectionNode, label: &str) -> Option<String> {
    let pressable = node.semantics.as_ref().is_some_and(|access| {
        access.role == ui::a11y::Role::Button
            && !access.state.disabled
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

/// The test id of the first widget under `node` that publishes a button named `label` as a
/// control it paints itself (an empty state's action): the widget is what is activated.
fn painted_button_named(node: &ui::headless::InspectionNode, label: &str) -> Option<String> {
    let painted = node.semantics.as_ref().is_some_and(|access| {
        access.children.iter().any(|child| {
            child.role == ui::a11y::Role::Button
                && !child.state.disabled
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

/// One app's check under way.
struct AppCheck<'a> {
    dir: &'a Path,
    short: &'a str,
    meta: &'a tilolang::PackageMeta,
    sources: &'a source::SourceMap,
    dump: Option<(u32, u32)>,
    shots: Option<&'a Path>,
    themes: &'a [String],
    keys: &'a [String],
    gathered: BTreeMap<Identity, Finding>,
    /// Measures text as the window's pipeline does, for clipped text.
    text: &'a mut ui::TextRenderer,
    ran: usize,
    /// The views some layout showed a node of.
    shown: BTreeSet<String>,
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
        )
    }

    /// The views of types the scenario did not open, on a resource its window names; and the
    /// views the system shows on surfaces of its own rather than in a window of the app: a
    /// type's row view (a search result, a row of an agent's answer) and an action's proposal
    /// view (the card asking to run it), on a resource of the type they take. A view another
    /// layout showed already is not opened again.
    fn open_views(&mut self, staged: &mut stage::Staged, title: &str) -> Result<(), String> {
        for default in &self.meta.default_views {
            let Some(view) = self.meta.views.iter().find(|v| v.id == default.view) else {
                continue;
            };
            // The root's own view is the window's first column already.
            if self.shown.contains(&view.name) || self.meta.roots.contains(&default.ty) {
                continue;
            }
            let Some(resource) = staged.resources().into_iter().find(|r| r.ty == default.ty) else {
                continue;
            };
            staged.open(resource);
            self.run(staged, &format!("{title}, then a {} opened", view.name))?;
        }
        for view in surface_views(self.meta) {
            if self.shown.contains(&view.name) {
                continue;
            }
            let [param] = view.params.as_slice() else {
                continue;
            };
            // A parameter names its type as written; a resource carries the type's id.
            let Some(ty) = self.meta.types.iter().find(|t| t.name == param.ty) else {
                continue;
            };
            let Some(resource) = staged.resources().into_iter().find(|r| r.ty == ty.id) else {
                continue;
            };
            staged.open_with(&resource, &view.id);
            self.run(staged, &format!("{title}, then {} shown", view.name))?;
        }
        // The views a type's bar switches to beside the one it opens on, on a resource of the
        // type: a project laid out as a board as well as the list it opens on.
        let meta = self.meta;
        for (view, ty) in switch_views(meta) {
            if self.shown.contains(&view.name) {
                continue;
            }
            let Some(resource) = staged.resources().into_iter().find(|r| r.ty == ty.id) else {
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
            if !pressed && !self.keys.is_empty() && Some(viewport) == read_at {
                // The window a person reaches by keyboard leaves it laid out differently: a
                // group folded away, a control focused and wearing its ring.
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
        if self.dump == Some(viewport) {
            let json = serde_json::to_string_pretty(&inspection).map_err(|e| e.to_string())?;
            eprintln!("== {} {label} at {layout}\n{json}", self.meta.app);
            if let Some(dir) = self.shots
                && (layout.display_scale - 1.0).abs() < f64::EPSILON
                && (layout.text_scale - 1.0).abs() < f32::EPSILON
            {
                self.shoot(staged, layout, label, dir)?;
            }
        }
        self.note_shown(&inspection.root);
        // A view that ran but drew nothing has no widget; what the app sent still names it.
        if let Some(composed) = staged.composed() {
            self.note_shown_row(&composed);
        }
        let rows = staged.rows();
        for row in rows.values() {
            self.note_shown_row(row);
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
                staged.harness().render_png(&file);
            }
        }
        staged.lay_out(viewport, layout.theme(), layout.input(), 1.0)
    }

    /// Remember the views shown in `key`: the view the node is of, and every view whose call
    /// holds it.
    fn note_key(&mut self, key: &str) {
        for (app, view) in self.sources.views_in(key) {
            if app == self.meta.app {
                self.shown.insert(view.to_owned());
            }
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
    if !report.unreached.is_empty() {
        let _ = writeln!(
            out,
            "note: no layout showed {}: no scenario reaches them",
            report.unreached.join(", ")
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

/// The shell's windows as a person sees them, staged from its own scenarios through the UI
/// host's path: windows standing with their apps' content, one bar per window, filling the
/// strips the desk gives them, and the trailing parts of the top bar at its edge.
#[cfg(test)]
mod windows {
    use std::path::Path;
    use std::rc::Rc;

    use super::stage;
    use ui::kurbo::Rect;
    use ui::widget::Widget;

    fn packages() -> (String, Rc<stage::Packages>) {
        let shell = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/shell");
        let search = vec![shell.parent().expect("apps/tl").to_path_buf()];
        let (built, platform) = super::build(&shell, &search).expect("the shell compiles");
        let app = built.last().expect("the shell").compiled.meta.app.clone();
        let packages = Rc::new(stage::Packages(
            Rc::new(built.into_iter().map(|b| b.compiled).collect()),
            platform,
        ));
        (app, packages)
    }

    /// The tasks app and every package it uses, staged from `apps/tl/tasks`.
    fn tasks_packages() -> Rc<stage::Packages> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/tasks");
        let search = vec![dir.parent().expect("apps/tl").to_path_buf()];
        let (built, platform) = super::build(&dir, &search).expect("the tasks app compiles");
        Rc::new(stage::Packages(
            Rc::new(built.into_iter().map(|b| b.compiled).collect()),
            platform,
        ))
    }

    /// Every widget under `root` named `name`, with its rect, in drawing order.
    fn all<'a>(root: &'a dyn Widget, name: &str, out: &mut Vec<&'a dyn Widget>) {
        if root.name() == name {
            out.push(root);
        }
        for child in root.children() {
            all(child, name, out);
        }
    }

    fn named<'a>(root: &'a dyn Widget, name: &str) -> Vec<&'a dyn Widget> {
        let mut out = Vec::new();
        all(root, name, &mut out);
        out
    }

    /// The window's own tabs under `root`: the strip a `TabStrip` draws (it draws as the
    /// toolkit's bar), never a tab bar an app puts in its own content.
    fn tab_strips(root: &dyn Widget) -> Vec<&dyn Widget> {
        named(root, "TabBar")
            .into_iter()
            .filter(|bar| is_tab_strip(*bar))
            .collect()
    }

    /// Whether `bar` is a window's own tabs rather than a tab bar an app put in its content.
    fn is_tab_strip(bar: &dyn Widget) -> bool {
        match bar.as_any() {
            Some(any) => any.is::<ui::widget::TabStrip>(),
            None => false,
        }
    }

    /// How many places the tab strip `bar` shows.
    fn tabs_of(bar: &dyn Widget) -> usize {
        bar.as_any()
            .and_then(|any| any.downcast_ref::<ui::widget::TabStrip>())
            .map_or(0, |strip| strip.bar().tabs().len())
    }

    #[test]
    fn its_windows_show_their_apps_content_under_one_bar_each() {
        tilo_ui_host::i18n::ensure();
        let (app, packages) = packages();
        let staged = stage::Staged::over(
            &app,
            &packages,
            "the traditional desktop",
            (1280, 800),
            ui::Theme::light(),
            None,
        )
        .expect("the scenario stages");
        let root = staged.harness().root_widget();
        let frames = named(root, "WindowFrame");
        assert_eq!(frames.len(), 3, "two notes and a channel stand on the desk");
        for frame in frames {
            let window = frame.get_rect();
            // What the window holds is cut at its edge, so a list scrolled past its top is
            // never read over the window behind it.
            assert_eq!(
                frame.child_clip_rect(),
                Some(window),
                "the window clips what it holds"
            );
            // The window shows its resource through its type's own view, filling it.
            let shells = named(frame, "NavShell");
            let [shell] = shells[..] else {
                panic!("one app window in each frame, not {}", shells.len());
            };
            let content = shell.get_rect();
            assert!(
                content.height() > window.height() * 0.8,
                "the app's window fills the frame: {content:?} in {window:?}"
            );
            // The frame's controls stand in the app's own header row, at its trailing end,
            // so the window has one bar: no bar of the frame's above the app's.
            assert!(
                (content.y0 - window.y0).abs() < 1.0,
                "no bar of the frame's stands over the app's header: {content:?} in {window:?}"
            );
            let buttons = named(frame, "Button");
            let close = buttons
                .iter()
                .map(|b| b.get_rect())
                .max_by(|a, b| a.x1.total_cmp(&b.x1))
                .expect("the frame's close");
            assert!(
                close.y1 < window.y0 + 64.0 && window.x1 - close.x1 < 24.0,
                "the close stands at the end of the window's bar: {close:?} in {window:?}"
            );
            let overlapping: Vec<Rect> = buttons
                .iter()
                .map(|b| b.get_rect())
                .filter(|b| *b != close && b.intersect(close).area() > 0.0)
                .collect();
            assert!(
                overlapping.is_empty(),
                "the app's header keeps room for the frame's controls: {overlapping:?}"
            );
        }
    }

    #[test]
    fn the_strips_windows_fill_their_strip_and_hold_their_apps_under_one_bar() {
        tilo_ui_host::i18n::ensure();
        let (app, packages) = packages();
        let staged = stage::Staged::over(
            &app,
            &packages,
            "a desk of places",
            (1280, 800),
            ui::Theme::light(),
            None,
        )
        .expect("the scenario stages");
        let frames = named(staged.harness().root_widget(), "WindowFrame");
        assert!(
            frames.len() >= 6,
            "the desk's strips stand as windows, not cards: {} frames",
            frames.len()
        );
        windows_fill_their_strips(&frames);

        // A strip of several places is one window whose bar is its tabs, the place in front
        // filling it under them; a strip of one has no tab bar above its app.
        let grouped = stage::Staged::over(
            &app,
            &packages,
            "a window of several places",
            (1280, 800),
            ui::Theme::light(),
            None,
        )
        .expect("the scenario stages");
        let group = named(grouped.harness().root_widget(), "WindowFrame");
        assert_eq!(group.len(), 2, "a window of places beside a window of one");
        the_places_of_a_strip_are_its_tabs(&group);

        // Two windows the desk holds stand side by side, each the share of the desk its strip's
        // width names, not the floor two of them would need: a row spreads across the desk while
        // its windows fit, and pans to them whole once they do not.
        let from = group
            .iter()
            .map(|frame| frame.get_rect().x0)
            .fold(f64::MAX, f64::min);
        let to = group
            .iter()
            .map(|frame| frame.get_rect().x1)
            .fold(f64::MIN, f64::max);
        assert!(
            to - from >= 1270.0,
            "the desk's row spreads across it at 1280: {} to {to}",
            from.round()
        );
    }

    #[test]
    fn a_window_alone_on_the_desk_takes_the_desk() {
        tilo_ui_host::i18n::ensure();
        let (app, packages) = packages();
        let staged = stage::Staged::over(
            &app,
            &packages,
            "a window alone on the desk",
            (1280, 800),
            ui::Theme::light(),
            None,
        )
        .expect("the scenario stages");
        let frames = named(staged.harness().root_widget(), "WindowFrame");
        assert_eq!(
            frames.len(),
            1,
            "one window on the desk, not {}",
            frames.len()
        );
        let window = frames[0].get_rect();
        assert!(
            window.width() > 1200.0,
            "a lone window takes the desk's width at 1280: {window:?}"
        );
        bar_and_content_in(frames[0], window);
    }

    /// A window is a window: it fills its strip's whole room, and every window of the desk
    /// shares that height, as the chrome's groups do. A card is a few hundred pixels tall with
    /// a sliver of its app under it.
    fn windows_fill_their_strips(frames: &[&dyn Widget]) {
        // The desk's own windows: the overlays the shell stages beside them are frames too.
        let frames: Vec<&dyn Widget> = frames
            .iter()
            .copied()
            .filter(|frame| frame.get_rect().height() > 100.0)
            .collect();
        let height = frames
            .iter()
            .map(|f| f.get_rect().height())
            .fold(0.0, f64::max);
        assert!(
            height > 500.0,
            "a window fills the desk's height: {height} at 1280x800"
        );
        for frame in &frames {
            let window = frame.get_rect();
            assert!(
                (window.height() - height).abs() < 1.0,
                "every window of the desk is as tall as the desk: {window:?} beside {height}"
            );
            // Its share of the row, never narrower than the chrome's own floor: a row of more
            // windows than the desk holds pans to them whole.
            assert!(
                window.width() >= 160.0,
                "a window keeps its share of the row: {window:?}"
            );
            bar_and_content_in(*frame, window);
        }
    }

    /// One bar per window, whichever state it is in — the tabs of a window of several places,
    /// or the app's own header row — and its app's content under that, filling the rest.
    fn bar_and_content_in(frame: &dyn Widget, window: Rect) {
        let strips = tab_strips(frame);
        let shells = named(frame, "NavShell");
        if let [tabs] = strips[..] {
            let bar = tabs.get_rect();
            assert!(
                bar.height() > 40.0 && bar.height() < 56.0,
                "the tabs are one window bar tall: {bar:?}"
            );
            assert!(
                bar.y0 - window.y0 < 1.0,
                "the tabs are the window's own bar, at its top edge: {bar:?} in {window:?}"
            );
            if let [shell] = shells[..] {
                assert!(
                    shell.get_rect().y0 >= bar.y1 - 1.0,
                    "the app stands under the tabs, not over them: {:?} under {bar:?}",
                    shell.get_rect()
                );
            }
        }
        for shell in shells {
            let content = shell.get_rect();
            assert!(
                content.y0 - window.y0 < 56.0,
                "one bar stands over the app, never two: {content:?} in {window:?}"
            );
            assert!(
                window.y1 - content.y1 < 1.0,
                "the app fills the window to its foot: {content:?} in {window:?}"
            );
        }
    }

    /// A strip of several places shows them as one window's tabs, the place in front under
    /// them; the strip of one shows its app from the window's own edge.
    fn the_places_of_a_strip_are_its_tabs(group: &[&dyn Widget]) {
        for frame in group {
            let window = frame.get_rect();
            let strips = tab_strips(*frame);
            match &strips[..] {
                [] => {}
                [bar] => {
                    let tabs = bar.get_rect();
                    assert!(
                        (tabs.y0 - window.y0).abs() < 1.0 && (tabs.x0 - window.x0).abs() < 1.0,
                        "the tabs are the window's own bar, at its top leading corner: {tabs:?} in {window:?}"
                    );
                    assert!(
                        tabs.height() > 40.0 && tabs.height() < 56.0,
                        "the tabs are one window bar tall: {tabs:?}"
                    );
                    assert!(
                        tabs.width() > window.width() / 2.0,
                        "the tabs are the window's bar, not a chip in it: {tabs:?} in {window:?}"
                    );
                    let items = tabs_of(*bar);
                    assert!(
                        items >= 3,
                        "every place of the strip is a tab: {items} tabs"
                    );
                    let shells = named(*frame, "NavShell");
                    let [shell] = shells[..] else {
                        panic!("one app in front of the tabs, not {}", shells.len());
                    };
                    let content = shell.get_rect();
                    assert!(
                        content.y0 >= tabs.y1 - 1.0 && window.y1 - content.y1 < 1.0,
                        "the place in front fills the window under its bar: {content:?} in {window:?}"
                    );
                }
                _ => panic!("one tab bar to a window, not {}", strips.len()),
            }
        }
        assert_eq!(
            group
                .iter()
                .filter(|frame| !tab_strips(**frame).is_empty())
                .count(),
            1,
            "the strip of several places shows its tabs"
        );
    }

    /// The controls under `root` outside any window, with their names: the shell's own.
    fn shell_controls(root: &dyn Widget, out: &mut Vec<(String, Rect)>) {
        if root.name() == "WindowFrame" {
            return;
        }
        if matches!(root.name(), "Button" | "MenuButton" | "Badge" | "Avatar") {
            out.push((root.name().to_owned(), root.get_rect()));
        }
        // A count a control carries on its icon, and that icon.
        for (part, rect) in root.test_rects() {
            if part == "count" || part == "icon" {
                out.push((format!("{} {part}", root.name()), rect));
            }
        }
        for child in root.children() {
            shell_controls(child, out);
        }
    }

    #[test]
    fn its_top_bar_is_one_row_whose_parts_never_press_into_each_other() {
        tilo_ui_host::i18n::ensure();
        let (app, packages) = packages();
        for (width, height, text) in [(1280, 800, 1.0), (768, 1024, 1.0), (320, 568, 1.5)] {
            let staged = stage::Staged::over(
                &app,
                &packages,
                "the traditional desktop",
                (width, height),
                ui::Theme::light().with_text_scale(text),
                None,
            )
            .expect("the scenario stages");
            let mut controls = Vec::new();
            shell_controls(staged.harness().root_widget(), &mut controls);
            let at = format!("{width}×{height} text {text}");
            // The bar is the band the system menu stands in; the dock's are lower down.
            let menu = controls
                .iter()
                .filter(|(n, _)| matches!(n.as_str(), "Button" | "MenuButton" | "Avatar"))
                .map(|(_, r)| *r)
                .min_by(|a, b| a.y0.total_cmp(&b.y0).then(a.x0.total_cmp(&b.x0)))
                .expect("the system menu");
            let bar: Vec<&(String, Rect)> = controls
                .iter()
                .filter(|(_, r)| r.y0 < menu.y1 && r.y1 > menu.y0)
                .collect();
            let pressed: Vec<Rect> = bar
                .iter()
                .filter(|(n, _)| matches!(n.as_str(), "Button" | "MenuButton" | "Avatar"))
                .map(|(_, r)| *r)
                .collect();
            for r in &pressed {
                assert!(
                    r.x0 >= 0.0 && r.x1 <= f64::from(width),
                    "{r:?} stays on the output at {at}"
                );
                // One row: every part shares the system menu's band.
                assert!(
                    (r.center().y - menu.center().y).abs() < 2.0,
                    "{r:?} stands in the one row at {at}: {bar:?}"
                );
                for other in &pressed {
                    assert!(
                        r == other || r.intersect(*other).area() <= 0.0,
                        "{r:?} presses into {other:?} at {at}"
                    );
                }
            }
            assert_eq!(
                bar.iter().filter(|(n, _)| n == "Button count").count(),
                1,
                "the bell's count, the bar's one, at {at}: {bar:?}"
            );
        }
    }

    #[test]
    fn a_count_stands_on_its_icons_corner_and_over_no_other_control() {
        tilo_ui_host::i18n::ensure();
        let (app, packages) = packages();
        for (width, height, text) in [(1280, 800, 1.0), (768, 1024, 1.0), (320, 568, 1.5)] {
            let staged = stage::Staged::over(
                &app,
                &packages,
                "the traditional desktop",
                (width, height),
                ui::Theme::light().with_text_scale(text),
                None,
            )
            .expect("the scenario stages");
            let mut controls = Vec::new();
            shell_controls(staged.harness().root_widget(), &mut controls);
            let at = format!("{width}×{height} text {text}");
            let pressed: Vec<Rect> = controls
                .iter()
                .filter(|(n, _)| matches!(n.as_str(), "Button" | "MenuButton" | "Avatar"))
                .map(|(_, r)| *r)
                .collect();
            let counts: Vec<(Rect, Rect)> = controls
                .windows(2)
                .filter_map(|pair| match pair {
                    [(count, c), (icon, i)]
                        if count.ends_with(" count") && icon.ends_with(" icon") =>
                    {
                        Some((*c, *i))
                    }
                    _ => None,
                })
                .collect();
            // The bell's and the mail's in the dock.
            assert_eq!(
                counts.len(),
                2,
                "the bell's count and the dock's at {at}: {controls:?}"
            );
            for (count, icon) in counts {
                // Over its own control alone: never the one beside it.
                let own = pressed
                    .iter()
                    .find(|r| r.contains(icon.center()))
                    .expect("the control the icon is on");
                // Centred on the icon's top trailing corner, as an app icon's count stands, and
                // held inside its own face where that corner would take it past the face's edge.
                let corner = ui::kurbo::Point::new(icon.x1, icon.y0);
                let wanted = Rect::from_center_size(corner, count.size());
                let left = wanted
                    .x0
                    .clamp(own.x0, (own.x1 - wanted.width()).max(own.x0));
                let top = wanted
                    .y0
                    .clamp(own.y0, (own.y1 - wanted.height()).max(own.y0));
                let expected = Rect::new(left, top, left + wanted.width(), top + wanted.height());
                assert!(
                    (count.center() - expected.center()).hypot() < 1.0,
                    "the count {count:?} stands on the corner of its icon {icon:?} inside its face {own:?} at {at}"
                );
                assert!(
                    count.x0 >= 0.0 && count.x1 <= f64::from(width) && count.y0 >= 0.0,
                    "the count {count:?} stays on the output at {at}"
                );
                for other in pressed.iter().filter(|r| *r != own) {
                    assert!(
                        other.intersect(count).area() <= 0.0,
                        "the count {count:?} stands over {other:?} at {at}"
                    );
                }
            }
        }
    }

    #[test]
    fn on_a_phone_the_window_in_front_fills_the_room_between_the_bar_and_the_dock() {
        tilo_ui_host::i18n::ensure();
        let (app, packages) = packages();
        let staged = stage::Staged::over(
            &app,
            &packages,
            "the traditional desktop",
            (320, 568),
            ui::Theme::light(),
            None,
        )
        .expect("the scenario stages");
        let root = staged.harness().root_widget();
        let frames = named(root, "WindowFrame");
        let [frame] = frames[..] else {
            panic!("the window in front alone, not {} windows", frames.len());
        };
        let said = {
            let mut said = Vec::new();
            read_out(&staged.harness().inspection().root, false, &mut said);
            said
        };
        assert!(
            said.iter()
                .any(|(in_window, name)| *in_window && name == "Q3 plan"),
            "the window is the one in front: {said:?}"
        );
        let chrome = named(root, "Chrome");
        let bar = chrome
            .iter()
            .map(|c| c.get_rect())
            .min_by(|a, b| a.y0.total_cmp(&b.y0))
            .expect("the top bar");
        let dock = chrome
            .iter()
            .map(|c| c.get_rect())
            .max_by(|a, b| a.y1.total_cmp(&b.y1))
            .expect("the dock");
        let window = frame.get_rect();
        assert!(
            window.y0 >= bar.y1 && window.y1 <= dock.y0,
            "the window {window:?} stands between the bar {bar:?} and the dock {dock:?}"
        );
        // It fills that room: no more than the gaps between them is left over.
        let gap = 16.0;
        assert!(
            window.y0 - bar.y1 <= gap && dock.y0 - window.y1 <= gap,
            "the window {window:?} fills the room between {bar:?} and {dock:?}"
        );
        assert!(
            window.x0 >= 0.0 && window.x1 <= 320.0 && window.width() >= 320.0 - 2.0 * gap,
            "the window {window:?} spans the output"
        );
    }

    /// What assistive technology reads under `node` (names and values), each with whether it
    /// stands in a window.
    fn read_out(
        node: &ui::headless::InspectionNode,
        in_window: bool,
        out: &mut Vec<(bool, String)>,
    ) {
        let in_window = in_window || node.widget == "WindowFrame";
        if let Some(access) = &node.semantics {
            for said in [&access.name, &access.value].into_iter().flatten() {
                out.push((in_window, said.clone()));
            }
        }
        for child in &node.children {
            read_out(child, in_window, out);
        }
    }

    /// What the traditional desktop reads out at `size`, laid out for a finger on a phone's
    /// width (its controls a finger's target), as the layout check lays a phone out.
    fn desktop_read_out(size: (u32, u32)) -> Vec<(bool, String)> {
        tilo_ui_host::i18n::ensure();
        let (app, packages) = packages();
        let mut staged = stage::Staged::over(
            &app,
            &packages,
            "the traditional desktop",
            size,
            ui::Theme::light(),
            None,
        )
        .expect("the scenario stages");
        let input = if size.0 < 600 {
            ui::interaction::InteractionEnvironment::TOUCH
        } else {
            ui::interaction::InteractionEnvironment::POINTER
        };
        staged
            .lay_out(size, ui::Theme::light(), input, 1.0)
            .expect("the desktop lays out");
        let mut said = Vec::new();
        read_out(&staged.harness().inspection().root, false, &mut said);
        said
    }

    /// Whether `node` is a shell header's overflow door, whatever language the window it
    /// frames reads in: the door is known by its id, not by the words on it.
    fn is_shell_door(node: &ui::headless::InspectionNode) -> bool {
        let [_, _, w, h] = node.rect;
        w > 0.0
            && h > 0.0
            && node
                .test_id
                .as_deref()
                .is_some_and(|id| id.starts_with("nav-shell-more-"))
    }

    /// Whether `node` is the "…" beside a page's title: the menu the page keeps its own
    /// actions behind when it is a window of one thing rather than a bar over columns.
    fn is_title_door(node: &ui::headless::InspectionNode) -> bool {
        let [_, _, w, h] = node.rect;
        w > 0.0
            && h > 0.0
            && node.widget.as_str() == "MenuButton"
            && node
                .test_id
                .as_deref()
                .is_some_and(|id| id.ends_with("~more"))
    }

    /// The menus each window under `node` offers, one list per window: a column of the bar
    /// holds the rest of the window's actions in one menu, or a page of one thing holds them
    /// behind the "…" beside its title, and never both.
    fn menus_per_window(node: &ui::headless::InspectionNode, out: &mut Vec<Vec<String>>) {
        if matches!(node.widget.as_str(), "WindowFrame" | "TilolangWindow") {
            out.push(Vec::new());
        }
        if is_shell_door(node)
            && let (Some(doors), Some(id)) = (out.last_mut(), node.test_id.as_deref())
            && let Some(column) = id.strip_prefix("nav-shell-more-")
        {
            doors.push(column.to_owned());
        } else if is_title_door(node)
            && let Some(doors) = out.last_mut()
        {
            doors.push("title".to_owned());
        }
        for child in &node.children {
            menus_per_window(child, out);
        }
    }

    /// The window's menu on the page: the bar's door or the "…" beside a title, whichever
    /// the window draws. Its items are the host's, read where the view is built; here it must
    /// stand where the person reaches it.
    fn shown_menu(node: &ui::headless::InspectionNode) -> Option<&ui::headless::InspectionNode> {
        if is_shell_door(node) || is_title_door(node) {
            return Some(node);
        }
        node.children.iter().find_map(shown_menu)
    }

    #[test]
    fn a_windows_header_holds_the_rest_of_its_actions_in_one_menu() {
        // A window draws its main actions as buttons and keeps the rest behind one menu: never
        // every action as a button of its own, one menu per column of its bar at most, and
        // one menu beside the title of the thing it shows, on a desk or on a phone. A window
        // of one thing keeps its commands beside that title, so no lone door stands on a bar
        // with nothing else to say.
        let columns = ["overview", "content", "sidebar"];
        for size in [(1280, 800), (390, 844)] {
            tilo_ui_host::i18n::ensure();
            let packages = tasks_packages();
            let mut staged = stage::Staged::over(
                "so.tilo.lang.tasks",
                &packages,
                "a busy week",
                size,
                ui::Theme::light(),
                None,
            )
            .expect("the scenario stages");
            let input = if size.0 < 600 {
                ui::interaction::InteractionEnvironment::TOUCH
            } else {
                ui::interaction::InteractionEnvironment::POINTER
            };
            staged
                .lay_out(size, ui::Theme::light(), input, 1.0)
                .expect("the app lays out");
            let mut windows = Vec::new();
            menus_per_window(&staged.harness().inspection().root, &mut windows);
            assert!(!windows.is_empty(), "the app shows a window at {size:?}");
            let mut menus = 0;
            for (window, doors) in windows.iter().enumerate() {
                for door in doors {
                    assert!(
                        columns.contains(&door.as_str()) || door == "title",
                        "window {window} heads a menu of {door:?} at {size:?}"
                    );
                    assert!(
                        doors.iter().filter(|held| *held == door).count() == 1,
                        "window {window} heads a second menu of {door:?} at {size:?}"
                    );
                }
                menus += doors.len();
            }
            assert!(
                menus > 0,
                "a window keeps the rest of its actions in a menu at {size:?}"
            );
        }
    }

    /// A control named `name` that stands on the window (a rect with room), by its test id.
    fn shown_named(node: &ui::headless::InspectionNode, name: &str) -> Option<String> {
        let [_, _, w, h] = node.rect;
        if w > 0.0
            && h > 0.0
            && node.semantics.as_ref().and_then(|s| s.name.as_deref()) == Some(name)
            && let Some(id) = &node.test_id
        {
            return Some(id.clone());
        }
        node.children
            .iter()
            .find_map(|child| shown_named(child, name))
    }

    #[test]
    fn an_action_without_an_icon_is_reachable_from_its_window_where_nothing_lists_it() {
        // In the strips preset a window is the app's own: nothing lists its actions, so its
        // bar holds the notes app's icon-less Untag as a labelled button, or its menu does
        // when the row has no room for it.
        tilo_ui_host::i18n::ensure();
        let (_, packages) = packages();
        for size in [(1280, 800), (390, 844)] {
            let mut staged = stage::Staged::over(
                "so.tilo.lang.notes",
                &packages,
                "a week of notes",
                size,
                ui::Theme::light(),
                None,
            )
            .expect("the notes stage");
            let input = if size.0 < 600 {
                ui::interaction::InteractionEnvironment::TOUCH
            } else {
                ui::interaction::InteractionEnvironment::POINTER
            };
            staged
                .lay_out(size, ui::Theme::light(), input, 1.0)
                .expect("the notes lay out");
            let root = staged.harness().inspection().root;
            if shown_named(&root, "Untag…").is_some() {
                continue;
            }
            let door = shown_menu(&root)
                .unwrap_or_else(|| panic!("Untag… is neither shown nor behind a menu at {size:?}"));
            let [_, _, w, h] = door.rect;
            assert!(
                w > 0.0 && h > 0.0,
                "the window's menu stands where the person reaches it at {size:?}"
            );
            assert!(
                door.semantics
                    .as_ref()
                    .and_then(|said| said.name.as_deref())
                    .is_some_and(|name| !name.is_empty()),
                "the window's menu is named at {size:?}"
            );
        }
    }

    #[test]
    fn its_people_are_called_by_their_names_never_their_ids() {
        let said = desktop_read_out((1280, 800));
        let ids: Vec<&String> = said
            .iter()
            .map(|(_, name)| name)
            .filter(|name| name.contains("scenario:"))
            .collect();
        assert!(ids.is_empty(), "people read as ids: {ids:?}");
        assert!(
            said.iter().any(|(_, name)| name.contains("Leo")),
            "the channel names who spoke: {said:?}"
        );
    }

    #[test]
    fn a_scenario_on_two_layers_is_named_apart() {
        // The shots of the window's own layer and of the shell's settings layer must not land
        // in one file, or the desk's shot is the settings page.
        assert_eq!(
            super::on_layer("the traditional desktop", None),
            "the traditional desktop"
        );
        assert_ne!(
            super::on_layer("the traditional desktop", Some("Shell")),
            super::on_layer("the traditional desktop", None)
        );
    }

    #[test]
    fn a_scenario_that_fails_is_said_rather_than_shown_empty() {
        tilo_ui_host::i18n::ensure();
        let (app, packages) = packages();
        let failed = stage::Staged::over(
            &app,
            &packages,
            "no such scenario",
            (1280, 800),
            ui::Theme::light(),
            None,
        );
        let Err(why) = failed else {
            panic!("a scenario that does not run is an error");
        };
        assert!(why.contains("no such scenario"), "{why}");
    }
}

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

    /// The cards `node` holds that stand whole inside `clip`, in drawing order.
    fn whole_cards(node: &dyn Widget, clip: Rect, out: &mut Vec<Rect>) {
        let rect = node.get_rect();
        if node.name() == "Card" && inside(clip, rect) {
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

    /// The pane is a board's whole width, and the lanes keep a gutter at each end of it: in a
    /// window no wider than three card measures and the gaps between them, all three lanes
    /// stand whole, so no lane's heading is cut by the view that scrolls the board.
    #[test]
    fn every_lane_of_a_board_stands_inside_the_pane() {
        tilo_ui_host::i18n::ensure();
        let packages = packages();
        let staged = stage::Staged::over(
            BOARD,
            &packages,
            "a board",
            (864, 800),
            ui::Theme::light(),
            None,
        )
        .expect("the board stages");
        let root = staged.harness().root_widget();
        let mut scrolls = Vec::new();
        named(root, "Scrollable", &mut scrolls);
        let across = scrolls
            .into_iter()
            .find(|scroll| {
                matches!(
                    scroll.layout_info(),
                    Some(ui::widget::LayoutInfo::Scroll(ui::widget::ScrollLayout {
                        direction: ui::widget::ScrollDirection::Horizontal,
                    }))
                )
            })
            .expect("the board scrolls across");
        let pane = across
            .child_clip_rect()
            .unwrap_or_else(|| across.get_rect());
        let lanes: Vec<ui::kurbo::Rect> = across
            .children()
            .iter()
            .flat_map(|content| content.children())
            .map(Widget::get_rect)
            .collect();
        assert_eq!(lanes.len(), 3, "three lanes: {lanes:?}");
        for lane in &lanes {
            assert!(
                inside(pane, *lane),
                "a lane stands whole in the pane at rest: {lane:?} in {pane:?}"
            );
        }
        let start = lanes[0].x0 - pane.x0;
        let end = pane.x1 - lanes[2].x1;
        assert!(
            start > 1.0,
            "the first lane keeps clear of the pane's edge: {pane:?}"
        );
        assert!(
            (start - end).abs() < 1.0,
            "the lanes keep the same gutter at both ends of the pane: {start} before, {end} after"
        );
    }
}
