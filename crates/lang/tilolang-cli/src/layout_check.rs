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

/// Display scales: layout is in logical pixels, so these change only what is snapped to device
/// pixels and how input is scaled.
pub const DISPLAY_SCALES: [f64; 2] = [1.0, 2.0];

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
        let (app_findings, ran, app_unreached) = check_app(dir, &options.search, options.dump)?;
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

/// One app's check under way.
struct AppCheck<'a> {
    dir: &'a Path,
    short: String,
    meta: &'a tilolang::PackageMeta,
    sources: source::SourceMap,
    dump: Option<(u32, u32)>,
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
            let theme = ui::Theme::light().with_text_scale(layout.text_scale);
            self.text.set_text_scale(theme.text_scale);
            self.text.apply_typography(&theme.tokens.typography);
            let input = if layout.size.touch {
                InteractionEnvironment::TOUCH
            } else {
                InteractionEnvironment::POINTER
            };
            let viewport = (layout.size.width, layout.size.height);
            staged.lay_out(viewport, theme, input, layout.display_scale)?;
            if let Some(why) = staged.failed() {
                return Err(format!(
                    "{}: {label}: the window refused what the app sent: {why}",
                    self.dir.display(),
                ));
            }
            self.ran = self.ran.saturating_add(1);
            let inspection = staged.harness().inspection();
            if self.dump == Some(viewport) {
                let json = serde_json::to_string_pretty(&inspection).map_err(|e| e.to_string())?;
                eprintln!("== {} {label} at {layout}\n{json}", self.meta.app);
            }
            self.note_shown(&inspection.root);
            let rows = staged.rows();
            for row in rows.values() {
                self.note_shown_row(row);
            }
            let mut problems = audit::inspect(
                &inspection.root,
                viewport,
                layout.size.touch.then(|| f64::from(input.minimum_target())),
            );
            problems.extend(audit::clipped_text(
                staged.harness_mut().root_widget_mut(),
                &mut self.text,
                viewport,
            ));
            for mut problem in problems {
                // A list draws a row as one widget keyed by the row: what it is about is the
                // row view's node, the deepest the row's tree holds.
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
        }
        Ok(())
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
