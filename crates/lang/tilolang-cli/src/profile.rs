//! `tilolang profile`: what an app costs to open and to use, in tilolang's own terms
//! ([`tilolang_tilo::runtime::cost`]): each unit of its code by the line it is written on, the calls
//! it ran, the fuel it spent and what it read from the host.
//!
//! A window is staged as `layout-check` stages it, over a workspace of `--seed` items as
//! `tilolang bench` seeds one, and each `--then` interaction is run as a person runs it: a key,
//! a click, typing, a scroll. What is counted is counted, not timed: the table is the same on
//! any machine at any load, so an app author can hold an app to a budget here. `--times` adds what
//! each step took in the scopes the framework marks (`tilo_resource::profiling`), the scopes a slow
//! event's log line names: where the time of a step went, on this machine.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use ui::interaction::InteractionEnvironment;

use crate::layout_check::stage;

#[derive(Clone, Debug)]
pub struct ProfileOptions {
    pub dir: PathBuf,
    pub search: Vec<PathBuf>,
    pub scenario: Option<String>,
    pub seed: Option<usize>,
    pub size: (u32, u32),
    /// The display's scale the window is laid out at: device pixels to a logical one.
    pub scale: f64,
    /// The layer of a package with more than one root to show (`Shell`, `Layout`).
    pub root: Option<String>,
    /// Interactions to run after the app is open, each in [`Interaction`]'s spelling.
    pub then: Vec<String>,
    /// Also time each step by the scopes the framework marks: what is timed depends on the machine,
    /// so it is printed beside the counts, never in place of them.
    pub times: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Interaction {
    /// A key press, as [`ui::headless::FrameHarness::key`] names one (`Tab`, `Down`, `Space`).
    Key(String),
    /// A click at a point of the window, in logical pixels: `click:120,300`.
    Click(f64, f64),
    /// Text typed where the window focuses it: `type:Shopping`.
    Type(String),
    /// A scroll by a distance, in logical pixels: `scroll:0,480`.
    Scroll(f64, f64),
    /// A click on the widget the window addresses by a name: `press:Controls`.
    Press(String),
    /// A pointer drag from one point to another, in logical pixels: `drag:260,400,420,400`.
    Drag((f64, f64), (f64, f64)),
    /// The pointer moved to a point with no button held, in logical pixels: `hover:640,980`.
    Hover(f64, f64),
    /// The window resized, as a person maximizing or dragging its edge does: `resize:1920x1080`.
    Resize(u32, u32),
}

impl Interaction {
    /// The interaction `spec` spells, as `--then` takes it: `key:Tab`, `click:120,300`,
    /// `type:Shopping`, `scroll:0,480`, `press:NAME`, `drag:260,400,420,400`, `hover:640,980`,
    /// `resize:1920x1080`.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let (what, rest) = spec.split_once(':').ok_or_else(|| {
            format!("--then {spec}: expected key:, click:, type:, scroll:, press:, drag:, hover: or resize:")
        })?;
        let pair = || -> Result<(f64, f64), String> {
            let (a, b) = rest
                .split_once(',')
                .ok_or_else(|| format!("--then {spec}: expected two numbers, as in 0,480"))?;
            let number = |text: &str| {
                text.trim()
                    .parse::<f64>()
                    .map_err(|_| format!("--then {spec}: {text:?} is not a number"))
            };
            Ok((number(a)?, number(b)?))
        };
        match what {
            "key" => Ok(Self::Key(rest.to_owned())),
            "press" => Ok(Self::Press(rest.to_owned())),
            "drag" => {
                let numbers = rest
                    .split(',')
                    .map(|text| text.trim().parse::<f64>())
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| {
                        format!("--then {spec}: expected four numbers, as in 260,400,420,400")
                    })?;
                let [x0, y0, x1, y1] = numbers[..] else {
                    return Err(format!(
                        "--then {spec}: expected four numbers, as in 260,400,420,400"
                    ));
                };
                Ok(Self::Drag((x0, y0), (x1, y1)))
            }
            "type" => Ok(Self::Type(rest.to_owned())),
            "click" => {
                let (x, y) = pair()?;
                Ok(Self::Click(x, y))
            }
            "scroll" => {
                let (dx, dy) = pair()?;
                Ok(Self::Scroll(dx, dy))
            }
            "hover" => {
                let (x, y) = pair()?;
                Ok(Self::Hover(x, y))
            }
            "resize" => rest
                .split_once('x')
                .and_then(|(w, h)| {
                    Some(Self::Resize(w.trim().parse().ok()?, h.trim().parse().ok()?))
                })
                .ok_or_else(|| format!("--then {spec}: expected a size, as in 1920x1080")),
            _ => Err(format!(
                "--then {spec}: {what:?} is not an interaction (key, click, type, scroll, press, drag, hover, resize)"
            )),
        }
    }

    /// The interaction as `--then` takes it: what a table is headed by.
    pub fn label(&self) -> String {
        match self {
            Self::Key(key) => format!("key:{key}"),
            Self::Click(x, y) => format!("click:{x},{y}"),
            Self::Type(text) => format!("type:{text}"),
            Self::Scroll(dx, dy) => format!("scroll:{dx},{dy}"),
            Self::Press(name) => format!("press:{name}"),
            Self::Drag((x0, y0), (x1, y1)) => format!("drag:{x0},{y0},{x1},{y1}"),
            Self::Hover(x, y) => format!("hover:{x},{y}"),
            Self::Resize(width, height) => format!("resize:{width}x{height}"),
        }
    }
}

/// Print what the app in `options.dir` costs to open, and then to do each of `options.then`, in the
/// language's own terms. # Errors A package does not read or compile, the scenario fails, or a
/// `--then` interaction does not spell one.
pub fn profile(options: &ProfileOptions) -> Result<(), String> {
    let interactions: Vec<Interaction> = options
        .then
        .iter()
        .map(|spec| Interaction::parse(spec))
        .collect::<Result<_, _>>()?;
    // The instant a run is of does not matter, but it must not move while it runs: the days a view
    // shows are read from it, as a layout check reads them.
    ui::clock::freeze_for_capture();
    tilo_app_window::i18n::ensure();
    let search = if options.search.is_empty() {
        options
            .dir
            .parent()
            .map(Path::to_path_buf)
            .into_iter()
            .collect::<Vec<_>>()
    } else {
        options.search.clone()
    };
    let started = Resident::now();
    let (built, published) =
        crate::compile(&options.dir, &search, &mut tilolang::OsEntropy::default())
            .map_err(|error| error.to_string())?;
    let compiled_at = Resident::now();
    let Some(app) = built.last().map(|built| built.compiled.meta.clone()) else {
        return Err(format!("{}: no package", options.dir.display()));
    };
    let compiled: Rc<Vec<tilolang_tilo::tilolang::Compiled>> =
        Rc::new(built.into_iter().map(|built| built.compiled).collect());
    let packages = Rc::new(stage::Packages::new(Rc::clone(&compiled), published));
    // The scenario named, else the app's first: `layout-check` checks every one, and a profile of
    // an app nobody stages is of the workspace before anything was made.
    let scenario = options
        .scenario
        .clone()
        .or_else(|| app.scenarios.first().map(|scenario| scenario.name.clone()));
    let viewport = options.size;
    let theme = ui::Theme::light();
    let input = InteractionEnvironment::POINTER;
    let (staged, opened) = tilo_resource::profiling::observe(|| -> Result<_, String> {
        let mut staged = stage::Staged::over_workspace(
            &app.app,
            &packages,
            viewport,
            theme.clone(),
            stage::Staging {
                scenario: scenario.clone(),
                root: options.root.clone(),
                seed: options.seed.unwrap_or(0),
                costs: true,
                states: Vec::new(),
            },
        )?;
        staged.lay_out(viewport, theme.clone(), input, options.scale)?;
        Ok(staged)
    });
    let mut staged = staged?;

    println!(
        "{} ({} items, scenario {})",
        app.app,
        options.seed.unwrap_or(0),
        scenario.as_deref().unwrap_or("none"),
    );
    print_costs(&staged, "open")?;
    if options.times {
        print_times(&opened);
        print_asking(&staged);
        print_fonts_read();
        print_resident(started, compiled_at);
    }
    for interaction in interactions {
        staged.reset_costs();
        let _ = ui::profiling::layout_counts::take();
        let _ = ui::profiling::layout_counts::take_restyled();
        let (done, scopes) = tilo_resource::profiling::observe(|| -> Result<(), String> {
            perform(&mut staged, &interaction)?;
            staged.answer()
        });
        done?;
        print_costs(&staged, &format!("then {}", interaction.label()))?;
        if options.times {
            print_times(&scopes);
            print_asking(&staged);
            print_fonts_read();
            println!("{:>10}  {}", "after", Resident::now());
        }
    }
    Ok(())
}

/// Do `interaction` in the staged window, as a person does it.
fn perform(staged: &mut stage::Staged, interaction: &Interaction) -> Result<(), String> {
    match interaction {
        Interaction::Key(key) => staged.key(key),
        Interaction::Click(x, y) => staged.harness_mut().click_at(*x, *y),
        Interaction::Type(text) => staged.harness_mut().type_text(text),
        Interaction::Scroll(dx, dy) => staged.harness_mut().scroll(*dx, *dy),
        Interaction::Press(name) => {
            staged.harness_mut().click_named(name).map_err(|error| {
                let mut names: Vec<_> = staged.harness_mut().targets().into_keys().collect();
                names.sort();
                format!(
                    "--then press:{name}: {error}; the window names {}",
                    names.join(", ")
                )
            })?;
        }
        Interaction::Drag(from, to) => staged.harness_mut().drag_at(*from, *to),
        Interaction::Hover(x, y) => staged.hover(*x, *y),
        Interaction::Resize(width, height) => staged
            .harness_mut()
            .resize((*width, *height))
            .map_err(|error| format!("--then resize:{width}x{height}: {error:?}"))?,
    }
    Ok(())
}

/// The scopes a step ran, the costliest first: microseconds, then how often. Scopes nest, so they
/// do not add up.
fn print_times(scopes: &[tilo_resource::profiling::Sample]) {
    let solved = ui::profiling::layout_counts::take();
    println!(
        "    layout  {} solves, {} passes, {} measures",
        solved.solves, solved.passes, solved.measures
    );
    let restyled = ui::profiling::layout_counts::take_restyled();
    if !restyled.is_empty() {
        let named: Vec<String> = restyled
            .iter()
            .map(|(name, count)| format!("{name} {count}"))
            .collect();
        println!("  restyled  {}", named.join(", "));
    }
    let mut scopes: Vec<_> = scopes.iter().collect();
    scopes.sort_by_key(|scope| std::cmp::Reverse(scope.elapsed_ns));
    println!("        µs     calls  scope");
    for scope in scopes.iter().take(24) {
        println!(
            "{:>10} {:>9}  {}",
            scope.elapsed_ns / 1_000,
            scope.calls,
            scope.phase
        );
    }
}

/// The widgets still asking for layout once a step settled: each lays the window out again on the
/// next input it handles.
fn print_asking(staged: &stage::Staged) {
    let asking = ui::profiling::layout_counts::asking_for_layout(staged.harness().root_widget());
    if !asking.is_empty() {
        let named: Vec<String> = asking
            .iter()
            .map(|(name, count)| format!("{name} {count}"))
            .collect();
        println!("    asking  {}", named.join(", "));
    }
}

/// What the process held as it started, once the app compiled and once it opened, and the files
/// mapped into it the most resident first.
fn print_resident(started: Resident, compiled: Resident) {
    for (when, resident) in [
        ("started", started),
        ("compiled", compiled),
        ("opened", Resident::now()),
    ] {
        println!("{when:>10}  {resident}");
    }
    for (kib, file) in mapped_files().into_iter().take(12) {
        println!("{:>10}  {file}", format!("{kib} KiB"));
    }
}

/// The font files text has read so far: a font is mapped up front and read only when text first
/// needs one of its faces.
fn print_fonts_read() {
    let read = ui::font_catalog::font_files_read();
    let names: Vec<String> = read
        .iter()
        .map(|path| {
            path.file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
        })
        .collect();
    println!("     fonts  {} read: {}", names.len(), names.join(", "));
}

fn print_costs(staged: &stage::Staged, interaction: &str) -> Result<(), String> {
    let costs = staged
        .costs()
        .ok_or("the window counts no costs: it was staged without them")?;
    println!();
    println!("== {interaction}");
    print!("{}", costs.render());
    Ok(())
}

/// The process's resident memory, from the kernel's own count (`/proc/self/smaps_rollup`):
/// what is held in RAM, split into the heap and other private memory (`anon`) and mapped files
/// (the program, fonts), and the most it held so far. Zero where the kernel does not say.
#[derive(Clone, Copy, Default)]
struct Resident {
    kib: u64,
    anon_kib: u64,
    file_kib: u64,
    peak_kib: u64,
}

impl Resident {
    fn now() -> Self {
        let field = |text: &str, name: &str| {
            text.lines()
                .find_map(|line| line.strip_prefix(name))
                .and_then(|rest| {
                    rest.trim()
                        .trim_end_matches("kB")
                        .trim()
                        .parse::<u64>()
                        .ok()
                })
                .unwrap_or(0)
        };
        let rollup = std::fs::read_to_string("/proc/self/smaps_rollup").unwrap_or_default();
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        let kib = field(&rollup, "Rss:");
        let anon_kib = field(&rollup, "Anonymous:");
        Self {
            kib,
            anon_kib,
            file_kib: kib.saturating_sub(anon_kib),
            peak_kib: field(&status, "VmHWM:"),
        }
    }
}

impl std::fmt::Display for Resident {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "resident {} MiB (anon {}, files {}), peak {} MiB",
            self.kib / 1024,
            self.anon_kib / 1024,
            self.file_kib / 1024,
            self.peak_kib / 1024
        )
    }
}

/// The files mapped into the process, the most resident first: KiB in RAM and the path.
fn mapped_files() -> Vec<(u64, String)> {
    let smaps = std::fs::read_to_string("/proc/self/smaps").unwrap_or_default();
    let mut files: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let mut current: Option<String> = None;
    for line in smaps.lines() {
        let mut fields = line.split_whitespace();
        let first = fields.next().unwrap_or_default();
        if first.contains('-') && first.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
            current = line
                .split_whitespace()
                .nth(5)
                .filter(|path| path.starts_with('/'))
                .map(str::to_owned);
        } else if first == "Rss:"
            && let Some(path) = &current
        {
            let kib: u64 = fields.next().and_then(|n| n.parse().ok()).unwrap_or(0);
            let held = files.entry(path.clone()).or_default();
            *held = held.saturating_add(kib);
        }
    }
    let mut files: Vec<(u64, String)> = files.into_iter().map(|(path, kib)| (kib, path)).collect();
    files.sort_by_key(|(kib, _)| std::cmp::Reverse(*kib));
    files
}
