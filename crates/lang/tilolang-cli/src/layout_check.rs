//! `tilolang layout-check` (docs/designs/app-language/layout.md, "Checking"): every view of an
//! app, staged from each of its scenarios, laid out headlessly through the UI host's path at
//! the sizes, display scales and text scales people use, and what is wrong with each layout
//! reported at the view's node in its source.
//!
//! Each scenario's window is one instance and one frame pipeline, laid out again for every
//! configuration; nothing is painted or rasterised.

mod audit;
mod source;
mod stage;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;

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
/// # Errors
/// An app does not compile, or its window refuses what it sends.
pub fn check(options: &Options) -> Result<Report, String> {
    let started = std::time::Instant::now();
    tilo_ui_host::i18n::ensure();
    let allow = options
        .allow
        .as_deref()
        .map(read_allow)
        .transpose()?
        .unwrap_or_default();
    let mut findings = Vec::new();
    let mut layouts_run: usize = 0;
    let mut unreached = Vec::new();
    for dir in &options.dirs {
        let (app_findings, ran, app_unreached) = check_app(
            dir,
            &options.search,
            options.dump,
            options.shots.as_deref(),
            &options.themes,
            options.root.as_deref(),
        )?;
        findings.extend(app_findings);
        layouts_run = layouts_run.saturating_add(ran);
        unreached.extend(app_unreached);
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

/// The identity of a finding while it is gathered: kind, app, view, node, widget.
type Identity = (Kind, String, String, Option<String>, String);

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

/// Check one app: every scenario at every layout, then each view no scenario showed, opened on
/// a resource of its type the window names. Its findings, the layouts run and the views
/// shown nowhere.
fn check_app(
    dir: &Path,
    search: &[PathBuf],
    dump: Option<(u32, u32)>,
    shots: Option<&Path>,
    themes: &[String],
    root: Option<&str>,
) -> Result<(Vec<Finding>, usize, Vec<String>), String> {
    let mut search = search.to_vec();
    search.extend(dir.parent().map(Path::to_path_buf));
    let (built, platform) = build(dir, &search)?;
    let sources = source::SourceMap::new(&built);
    let meta = built.last().ok_or("no package")?.compiled.meta.clone();
    let packages = Rc::new(stage::Packages(
        Rc::new(built.into_iter().map(|b| b.compiled).collect()),
        platform,
    ));
    let mut check = AppCheck {
        dir,
        short: meta.app.rsplit('.').next().unwrap_or(&meta.app).to_owned(),
        meta: &meta,
        sources,
        dump,
        shots,
        themes,
        gathered: BTreeMap::new(),
        text: ui::TextRenderer::new(),
        ran: 0,
        shown: BTreeSet::new(),
    };
    let first = layouts().first().copied().ok_or("no layouts")?;
    let roots = layers_of(&meta, root);
    for root in &roots {
        for scenario in &meta.scenarios {
            let mut staged = stage::Staged::over(
                &meta.app,
                &packages,
                &scenario.name,
                (first.size.width, first.size.height),
                ui::Theme::light(),
                root.as_deref(),
            )?;
            // A layer other than the window's own is named in what the check says and in its
            // shots' names, so the shots of one scenario on two layers never overwrite each
            // other.
            let title = on_layer(&scenario.name, root.as_deref());
            check.run(&mut staged, &title)?;
            // The views of types the scenario did not open, on a resource its window names.
            for default in &meta.default_views {
                let Some(view) = meta.views.iter().find(|v| v.id == default.view) else {
                    continue;
                };
                // The root's own view is the window's first column already.
                if check.shown.contains(&view.name) || meta.roots.contains(&default.ty) {
                    continue;
                }
                let Some(resource) = staged.resources().into_iter().find(|r| r.ty == default.ty)
                else {
                    continue;
                };
                staged.open(resource);
                check.run(
                    &mut staged,
                    &format!("{title}, then a {} opened", view.name),
                )?;
            }
            // The views the system shows on surfaces of its own rather than in a window of the
            // app: a type's row view (a search result, a row of an agent's answer) and an action's
            // proposal view (the card asking to run it), on a resource of the type they take.
            for view in surface_views(&meta) {
                if check.shown.contains(&view.name) {
                    continue;
                }
                let [param] = view.params.as_slice() else {
                    continue;
                };
                // A parameter names its type as written; a resource carries the type's id.
                let Some(ty) = meta.types.iter().find(|t| t.name == param.ty) else {
                    continue;
                };
                let Some(resource) = staged.resources().into_iter().find(|r| r.ty == ty.id) else {
                    continue;
                };
                staged.open_with(&resource, &view.id);
                check.run(&mut staged, &format!("{title}, then {} shown", view.name))?;
            }
            press_stage(
                &mut check,
                &meta,
                &packages,
                &scenario.name,
                root.as_deref(),
            )?;
        }
    }
    let unreached = meta
        .views
        .iter()
        .filter(|v| v.presents.is_none() && v.row_of.is_none())
        .filter(|v| !check.shown.contains(&v.name))
        .map(|v| format!("{} › {}", check.short, v.name))
        .collect();
    Ok((check.gathered.into_values().collect(), check.ran, unreached))
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

/// What the window shows once a person pressed one of its primary actions (a new note opened
/// beside the list, its header row laid out again around it): at each size, from `scenario` as
/// staged, pressed as a person presses (a tap on a touchscreen, a click otherwise) and with the
/// motion it starts run out, read as it stands then, before anything lays the window out
/// afresh. A layout that only a fresh pass puts right (a header row left where a frame of the
/// motion put it) shows here.
fn press_stage(
    check: &mut AppCheck<'_>,
    meta: &tilolang::PackageMeta,
    packages: &Rc<stage::Packages>,
    scenario: &str,
    root: Option<&str>,
) -> Result<(), String> {
    for label in primary_labels(meta) {
        for size in SIZES {
            let layout = Layout {
                size,
                display_scale: 1.0,
                text_scale: 1.0,
            };
            let viewport = (size.width, size.height);
            let mut staged = stage::Staged::over(
                &meta.app,
                packages,
                scenario,
                viewport,
                layout.theme(),
                root,
            )?;
            staged.lay_out_moving(viewport, layout.theme(), layout.input())?;
            let Some(id) = button_named(&staged.harness().inspection().root, label) else {
                continue;
            };
            staged.press(&id, size.touch)?;
            check.audit(
                &mut staged,
                layout,
                &format!("{}, then {label} pressed", on_layer(scenario, root)),
            )?;
        }
    }
    Ok(())
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

/// One app's check under way.
struct AppCheck<'a> {
    dir: &'a Path,
    short: String,
    meta: &'a tilolang::PackageMeta,
    sources: source::SourceMap,
    dump: Option<(u32, u32)>,
    shots: Option<&'a Path>,
    themes: &'a [String],
    gathered: BTreeMap<Identity, Finding>,
    /// Measures text as the window's pipeline does, for clipped text.
    text: ui::TextRenderer,
    ran: usize,
    /// The views some layout showed a node of.
    shown: BTreeSet<String>,
}

impl AppCheck<'_> {
    /// Lay `staged` out at every layout and gather what is wrong, as staged by `label`.
    fn run(&mut self, staged: &mut stage::Staged, label: &str) -> Result<(), String> {
        for layout in layouts() {
            staged.lay_out(
                (layout.size.width, layout.size.height),
                layout.theme(),
                layout.input(),
                layout.display_scale,
            )?;
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
            &mut self.text,
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
            self.short.clone(),
            view.clone(),
            node.clone(),
            problem.widget.clone(),
        );
        let shown = layout.to_string();
        let finding = self.gathered.entry(identity).or_insert_with(|| Finding {
            kind: problem.kind,
            app: self.short.clone(),
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

/// The shell's traditional desktop as a person sees it, staged from its own scenario through
/// the UI host's path: windows standing with their apps' content, one bar per window, and the
/// trailing parts of the top bar at its edge.
#[cfg(test)]
mod traditional_desktop {
    use std::path::Path;
    use std::rc::Rc;

    use ui::kurbo::Rect;
    use ui::widget::Widget;

    use super::stage;

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
                // Centred on the icon's top trailing corner, as an app icon's count stands.
                let corner = ui::kurbo::Point::new(icon.x1, icon.y0);
                assert!(
                    (count.center() - corner).hypot() < 1.0,
                    "the count {count:?} stands on the corner of its icon {icon:?} at {at}"
                );
                assert!(
                    count.x0 >= 0.0 && count.x1 <= f64::from(width) && count.y0 >= 0.0,
                    "the count {count:?} stays on the output at {at}"
                );
                // Over its own control alone: never the one beside it.
                let own = pressed
                    .iter()
                    .find(|r| r.contains(icon.center()))
                    .expect("the control the icon is on");
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

    #[test]
    fn a_windows_header_holds_the_apps_own_actions_and_no_generated_menu() {
        // The desktop's bar lists the window's actions (its window menu), so what does not
        // fit in a window's header gives way to it: no "More actions" door in a window, on a
        // desk or on a phone, and never a list made up for the header.
        for size in [(1280, 800), (320, 568)] {
            let said = desktop_read_out(size);
            let generated: Vec<&String> = said
                .iter()
                .filter(|(in_window, name)| *in_window && name == "More actions")
                .map(|(_, name)| name)
                .collect();
            assert!(
                generated.is_empty(),
                "a window offers {generated:?} at {size:?}"
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
        // header holds the notes app's icon-less Untag as a labelled button, or behind the
        // header's door when the row has no room for it.
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
            let door = shown_named(&root, "More actions")
                .unwrap_or_else(|| panic!("Untag… is neither shown nor behind a door at {size:?}"));
            staged.press(&door, size.0 < 600).expect("the door opens");
            let mut said = Vec::new();
            read_out(&staged.harness().inspection().root, false, &mut said);
            assert!(
                said.iter().any(|(_, name)| name == "Untag…"),
                "the door holds Untag… at {size:?}: {said:?}"
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
