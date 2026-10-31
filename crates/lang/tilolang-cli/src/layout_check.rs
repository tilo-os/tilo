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
    /// The shipped themes `shots` are taken in, by name (`glass`, `block`), each in its light
    /// and dark variant; none takes the stock light and dark.
    pub themes: Vec<String>,
}

/// The light and the dark variant of the shipped theme `name`: `default` the stock pair,
/// another name its preset and that preset's light sibling (`glass`, `glass light`).
fn theme_pair(name: &str) -> Result<[(&'static str, ui::Theme); 2], String> {
    if ui::theme::Preset::find(name) == Some(ui::theme::Preset::Dark) || name == "default" {
        return Ok([("light", ui::Theme::light()), ("dark", ui::Theme::dark())]);
    }
    let dark = ui::Theme::named(name).ok_or_else(|| format!("--theme {name}: no such theme"))?;
    let light = ui::Theme::named(&format!("{name} light")).unwrap_or_else(|| dark.clone());
    Ok([("light", light), ("dark", dark)])
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

/// Check one app: every scenario at every layout, then each view no scenario showed, opened on
/// a resource of its type the window names. Its findings, the layouts run and the views
/// shown nowhere.
fn check_app(
    dir: &Path,
    search: &[PathBuf],
    dump: Option<(u32, u32)>,
    shots: Option<&Path>,
    themes: &[String],
) -> Result<(Vec<Finding>, usize, Vec<String>), String> {
    let mut search = search.to_vec();
    search.extend(dir.parent().map(Path::to_path_buf));
    let dirs = tilolang::source::dependency_order(dir, &search)?;
    let built = tilolang::source::compile_dirs(&dirs, &[], &mut tilolang::OsEntropy::default())
        .map_err(|e| e.to_string())?;
    let sources = source::SourceMap::new(&built);
    let meta = built.last().ok_or("no package")?.compiled.meta.clone();
    let packages = Rc::new(stage::Packages(Rc::new(
        built.into_iter().map(|b| b.compiled).collect(),
    )));
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
    for scenario in &meta.scenarios {
        let mut staged = stage::Staged::new(
            &meta.app,
            &packages,
            &scenario.name,
            (first.size.width, first.size.height),
            ui::Theme::light(),
        )?;
        check.run(&mut staged, &scenario.name)?;
        // The views of types the scenario did not open, on a resource its window names.
        for default in &meta.default_views {
            let Some(view) = meta.views.iter().find(|v| v.id == default.view) else {
                continue;
            };
            // The root's own view is the window's first column already.
            if check.shown.contains(&view.name) || meta.roots.contains(&default.ty) {
                continue;
            }
            let Some(resource) = staged.resources().into_iter().find(|r| r.ty == default.ty) else {
                continue;
            };
            staged.open(resource);
            check.run(
                &mut staged,
                &format!("{}, then a {} opened", scenario.name, view.name),
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
            check.run(
                &mut staged,
                &format!("{}, then {} shown", scenario.name, view.name),
            )?;
        }
        press_stage(&mut check, &meta, &packages, &scenario.name)?;
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
) -> Result<(), String> {
    for label in primary_labels(meta) {
        for size in SIZES {
            let layout = Layout {
                size,
                display_scale: 1.0,
                text_scale: 1.0,
            };
            let viewport = (size.width, size.height);
            let mut staged =
                stage::Staged::new(&meta.app, packages, scenario, viewport, layout.theme())?;
            staged.lay_out_moving(viewport, layout.theme(), layout.input())?;
            let Some(id) = button_named(&staged.harness().inspection().root, label) else {
                continue;
            };
            staged.press(&id, size.touch)?;
            check.audit(
                &mut staged,
                layout,
                &format!("{scenario}, then {label} pressed"),
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
                    dir.join(format!("{}-{slug}-{family}-{name}.png", self.short))
                };
                staged.harness().render_png(&file);
            }
        }
        staged.lay_out(viewport, layout.theme(), layout.input(), 1.0)
    }

    /// Remember the views whose nodes `node` and what it holds show.
    fn note_shown(&mut self, node: &ui::headless::InspectionNode) {
        if let Some(origin) = node
            .test_id
            .as_deref()
            .and_then(|id| self.sources.origin(id))
            && origin.app == self.meta.app
        {
            self.shown.insert(origin.view);
        }
        for child in &node.children {
            self.note_shown(child);
        }
    }

    /// Remember the views a row's tree is of.
    fn note_shown_row(&mut self, node: &tilo_ui_host::UiNode) {
        if let Some(origin) = self.sources.origin(&node.key)
            && origin.app == self.meta.app
        {
            self.shown.insert(origin.view);
        }
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
