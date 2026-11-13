//! `tilolang profile`: what an app costs to open and to use, in tilolang's own terms
//! ([`tilolang_tilo::runtime::cost`]): each unit of its code by the line it is written on, the calls
//! it ran, the fuel it spent and what it read from the host.
//!
//! A window is staged as `layout-check` stages it, over a workspace of `--seed` items as
//! `tilolang bench` seeds one, and each `--then` interaction is run as a person runs it: a key,
//! a click, typing, a scroll. What is counted is counted, not timed: the table is the same on
//! any machine at any load, so an app author can hold an app to a budget here.

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
    /// The layer of a package with more than one root to show (`Desk`, `Arrangement`).
    pub root: Option<String>,
    /// Interactions to run after the app is open, each in [`Interaction`]'s spelling.
    pub then: Vec<String>,
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
}

impl Interaction {
    /// The interaction `spec` spells, as `--then` takes it: `key:Tab`, `click:120,300`,
    /// `type:Shopping`, `scroll:0,480`.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let (what, rest) = spec
            .split_once(':')
            .ok_or_else(|| format!("--then {spec}: expected key:, click:, type: or scroll:"))?;
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
            "type" => Ok(Self::Type(rest.to_owned())),
            "click" => {
                let (x, y) = pair()?;
                Ok(Self::Click(x, y))
            }
            "scroll" => {
                let (dx, dy) = pair()?;
                Ok(Self::Scroll(dx, dy))
            }
            _ => Err(format!(
                "--then {spec}: {what:?} is not an interaction (key, click, type, scroll)"
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
    tilo_ui_host::i18n::ensure();
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
    let (built, published) =
        crate::compile(&options.dir, &search, &mut tilolang::OsEntropy::default())
            .map_err(|error| error.to_string())?;
    let Some(app) = built.last().map(|built| built.compiled.meta.clone()) else {
        return Err(format!("{}: no package", options.dir.display()));
    };
    let compiled: Rc<Vec<tilolang_tilo::tilolang::Compiled>> =
        Rc::new(built.into_iter().map(|built| built.compiled).collect());
    let packages = Rc::new(stage::Packages(Rc::clone(&compiled), published));
    // The scenario named, else the app's first: `layout-check` checks every one, and a profile of
    // an app nobody stages is of the workspace before anything was made.
    let scenario = options
        .scenario
        .clone()
        .or_else(|| app.scenarios.first().map(|scenario| scenario.name.clone()));
    let record = options
        .seed
        .filter(|n| *n > 0)
        .map(|n| {
            let mut doc = tilolang_tilo::bench::seeded_record(&compiled, n)?;
            ui::automerge::AutoCommit::load(&doc.save())
                .map_err(|error| format!("the seeded workspace does not load: {error}"))
        })
        .transpose()?;
    let viewport = options.size;
    let theme = ui::Theme::light();
    let input = InteractionEnvironment::POINTER;
    let mut staged = stage::Staged::over_workspace(
        &app.app,
        &packages,
        viewport,
        theme.clone(),
        stage::Staging {
            scenario: scenario.clone(),
            root: options.root.clone(),
            record,
            costs: true,
            states: Vec::new(),
        },
    )?;
    staged.lay_out(viewport, theme.clone(), input, 1.0)?;

    println!(
        "{} ({} items, scenario {})",
        app.app,
        options.seed.unwrap_or(0),
        scenario.as_deref().unwrap_or("none"),
    );
    print_costs(&staged, "open")?;
    for interaction in interactions {
        staged.reset_costs();
        match &interaction {
            Interaction::Key(key) => staged.key(key),
            Interaction::Click(x, y) => {
                staged.harness_mut().click_at(*x, *y);
            }
            Interaction::Type(text) => staged.harness_mut().type_text(text),
            Interaction::Scroll(dx, dy) => staged.harness_mut().scroll(*dx, *dy),
        }
        staged.lay_out(viewport, theme.clone(), input, 1.0)?;
        print_costs(&staged, &format!("then {}", interaction.label()))?;
    }
    Ok(())
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
