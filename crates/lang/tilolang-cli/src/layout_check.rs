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
    /// Layouts run, over every scenario of every app.
    pub layouts: usize,
    pub elapsed_ms: u128,
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
    let mut layouts_run = 0;
    for dir in &options.dirs {
        let (app_findings, ran) = check_app(dir, &options.search, options.dump)?;
        findings.extend(app_findings);
        layouts_run += ran;
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
        layouts: layouts_run,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

/// The identity of a finding while it is gathered: kind, app, view, node, widget.
type Identity = (Kind, String, String, Option<String>, String);

/// Check one app: every scenario at every layout. Its findings and the layouts run.
fn check_app(
    dir: &Path,
    search: &[PathBuf],
    dump: Option<(u32, u32)>,
) -> Result<(Vec<Finding>, usize), String> {
    let mut search = search.to_vec();
    search.extend(dir.parent().map(Path::to_path_buf));
    let dirs = tilolang::source::dependency_order(dir, &search)?;
    let built = tilolang::source::compile_dirs(&dirs, &[], &mut tilolang::OsEntropy::default())
        .map_err(|e| e.to_string())?;
    let sources = source::SourceMap::new(&built);
    let meta = built.last().ok_or("no package")?.compiled.meta.clone();
    let short = meta.app.rsplit('.').next().unwrap_or(&meta.app).to_owned();
    let packages = Rc::new(stage::Packages(Rc::new(
        built.into_iter().map(|b| b.compiled).collect(),
    )));
    let mut gathered: BTreeMap<Identity, Finding> = BTreeMap::new();
    let mut text = ui::TextRenderer::new();
    let all = layouts();
    let mut ran = 0;
    for scenario in &meta.scenarios {
        let first = all.first().ok_or("no layouts")?;
        let mut staged = stage::Staged::new(
            &meta.app,
            &packages,
            &scenario.name,
            (first.size.width, first.size.height),
            ui::Theme::light(),
        )?;
        for layout in &all {
            let theme = ui::Theme::light().with_text_scale(layout.text_scale);
            text.set_text_scale(theme.text_scale);
            text.apply_typography(&theme.tokens.typography);
            let input = if layout.size.touch {
                InteractionEnvironment::TOUCH
            } else {
                InteractionEnvironment::POINTER
            };
            let viewport = (layout.size.width, layout.size.height);
            staged.lay_out(viewport, theme, input, layout.display_scale)?;
            if let Some(why) = staged.failed() {
                return Err(format!(
                    "{}: scenario {:?}: the window refused what the app sent: {why}",
                    dir.display(),
                    scenario.name
                ));
            }
            ran += 1;
            if dump == Some((layout.size.width, layout.size.height)) {
                let json = staged
                    .harness()
                    .inspection_json()
                    .map_err(|e| e.to_string())?;
                eprintln!("== {} {} at {layout}\n{json}", meta.app, scenario.name);
            }
            let mut problems = audit::inspect(
                &staged.harness().inspection().root,
                viewport,
                layout.size.touch.then(|| f64::from(input.minimum_target())),
            );
            problems.extend(audit::clipped_text(
                staged.harness_mut().root_widget_mut(),
                &mut text,
                viewport,
            ));
            for problem in problems {
                // The window's own parts (the shell's Back button) have test ids of their own,
                // not the compiler's: they are shown as the window's, by that id.
                let chrome = problem
                    .id
                    .as_ref()
                    .filter(|id| sources.origin(id).is_none());
                let origin = match chrome {
                    Some(_) => None,
                    None => sources.origin_of(&meta.app, &problem.keys),
                };
                let (view, node) = origin.as_ref().map_or_else(
                    || ("(window)".to_owned(), chrome.cloned()),
                    |o| (o.view.clone(), o.node.clone()),
                );
                let identity = (
                    problem.kind,
                    short.clone(),
                    view.clone(),
                    node.clone(),
                    problem.widget.clone(),
                );
                let shown = layout.to_string();
                let finding = gathered.entry(identity).or_insert_with(|| Finding {
                    kind: problem.kind,
                    app: short.clone(),
                    view,
                    node,
                    widget: problem.widget.clone(),
                    file: origin.as_ref().map(|o| o.span.file.clone()),
                    line: origin.as_ref().map(|o| o.span.line),
                    column: origin.as_ref().map(|o| o.span.column),
                    detail: problem.detail.clone(),
                    scenario: scenario.name.clone(),
                    layouts: Vec::new(),
                    count: 0,
                    id: String::new(),
                    allowed: false,
                });
                finding.count += 1;
                if !finding.layouts.contains(&shown) {
                    finding.layouts.push(shown);
                }
            }
        }
    }
    Ok((gathered.into_values().collect(), ran))
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
        out.push_str(&format!(
            "{place}{}{}: {} (at {first}{}; scenario {:?})\n",
            if finding.allowed { "allowed " } else { "" },
            finding.id,
            finding.detail,
            if more > 0 {
                format!(" and {more} more layouts")
            } else {
                String::new()
            },
            finding.scenario,
        ));
    }
    for stale in &report.stale {
        out.push_str(&format!(
            "note: the allow-list names `{stale}`, which no longer shows: take it out\n"
        ));
    }
    let new = report.new_findings().count();
    out.push_str(&format!(
        "{} findings ({new} not allowed) in {} layouts, {:.1} s\n",
        report.findings.len(),
        report.layouts,
        report.elapsed_ms as f64 / 1000.0
    ));
    out
}
