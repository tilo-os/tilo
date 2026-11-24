//! `tilo-layout-check`'s command line (`tools/layout-check`): its options read into [`Options`], the check run, its
//! report written, and the exit it ends with. A program that adds to what layouts stand on (sample
//! data of an outside service, registered with `tilolang_tilo::origin::register_sample`) runs the
//! same command through [`run`].

use std::path::PathBuf;
use std::process::ExitCode;

use super::{Dump, Options, check, render};

/// What the layout check takes and does, as `--help` prints it.
pub const USAGE: &str = "\
  tilo-layout-check [DIR]... [--search DIR]... [--allow FILE] [--strict] [--json] [--root NAME] [--state NAME=VALUE]... [--key KEY]... [--hover X,Y]... [--press LABEL]... [--dump|--dump-findings WxH [--shots DIR [--theme NAME]... [--bar-at-rest] [--shot-height H]]] [--locale TAG]
      lay out every view of each app headlessly, staged from each of its scenarios, at window
      sizes from 320x568 to 3440x1440, display scales 1 and 2 and text scales 1 and 1.5, and
      report overflow, clipped text, overlapping siblings, hit targets below the minimum and
      pages that scroll sideways, each at its view's node in the source. Exits 1 on a finding
      the allow-list (one finding id per line) does not name; --strict exits 1 on an allow-list
      line whose finding no longer shows and on a view no layout showed, too; --json prints the
      report as JSON;
      --dump prints each layout at that window size as the check reads it (boxes, clips and
      semantics of every widget) to stderr, for a run narrowed to the views it reads;
      --dump-findings prints only the layouts a finding was read from, which is what a check over
      every app asks for, and --shots writes the window at that size (display
      and text scale 1) as each stage leaves it, light and dark, to PNGs in DIR. --theme takes
      them in that shipped theme (default, glass, block, ...) instead of the stock pair, light and
      dark each. --bar-at-rest takes them as a maximized window stands at rest, its bar hidden
      until the person reaches for the top edge, instead of under the bar. --shot-height takes
      them in a window H tall at that width, so the lower part of a long page shows; the check
      itself still reads the window at its size. --root lays out the
      layer a package with more than one root names (the shell's `Shell`, the one with the bar
      and the dock), instead of the package's first root.
      --state stages a view state at a value the app's own code names (`place=Task.today`, a
      filter; `side=Side.right`, a case of its enum) before any window opens: the place a
      window stands on when nobody has picked one, as a scenario that laid it out would.
      --key sends a key press (`Tab`, `Down`, `Space`) to each window before it is read, in the
      order given, at the `--dump` size, so a shot and a dump show the layout a person reaches
      by keyboard. --hover rests the pointer at X,Y (logical pixels of the window) once the keys
      are sent, so a shot shows what pointing there shows. --locale lays every window out for a
      person reading in that language (`en-US`, `de-DE`; default en-US), whatever locale this
      machine runs in.\n";

/// Run the layout check with `args` (`--help` prints [`USAGE`]): 0 when the layouts
/// hold, 1 when a finding is new (or, with `--strict`, an allowed one no longer shows or a view
/// is reached by no scenario), 2 when the check could not run.
#[must_use]
pub fn run(args: &[String]) -> ExitCode {
    let mut options = Options::default();
    let mut json = false;
    let mut strict = false;
    let mut iter = args.iter();
    let parsed: Result<(), String> = (|| {
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "-h" | "--help" => return Err(String::new()),
                "--search" | "--path" => options.search.push(value(&mut iter, arg)?.into()),
                "--allow" => options.allow = Some(value(&mut iter, arg)?.into()),
                "--json" => json = true,
                "--strict" => strict = true,
                "--shots" => options.shots = Some(value(&mut iter, arg)?.into()),
                "--bar-at-rest" => options.bar_at_rest = true,
                "--shot-height" => {
                    let height = value(&mut iter, arg)?;
                    options.shot_height = Some(height.parse().map_err(|_| {
                        format!("--shot-height takes a height in logical pixels, not {height}")
                    })?);
                }
                "--theme" => options.themes.push(value(&mut iter, arg)?),
                "--root" => options.root = Some(value(&mut iter, arg)?),
                "--state" => options.states.push(value(&mut iter, arg)?),
                "--key" => options.keys.push(value(&mut iter, arg)?),
                "--hover" => options.hovers.push(value(&mut iter, arg)?),
                "--press" => options.presses.push(value(&mut iter, arg)?),
                "--locale" => options.locale = value(&mut iter, arg)?,
                "--dump" | "--dump-findings" => {
                    let size = value(&mut iter, arg)?;
                    let size = size
                        .split_once('x')
                        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                        .ok_or_else(|| format!("{arg} {size}: a window size such as 390x844"))?;
                    options.dump = Some(if arg == "--dump" {
                        Dump::Every(size)
                    } else {
                        Dump::Findings(size)
                    });
                }
                flag if flag.starts_with('-') => {
                    return Err(format!(
                        "unknown option {flag}; `tilo-layout-check --help` lists the options"
                    ));
                }
                dir => options.dirs.push(dir.into()),
            }
        }
        Ok(())
    })();
    match parsed {
        Err(error) if error.is_empty() => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("tilo-layout-check: {error}");
            return ExitCode::from(2);
        }
        Ok(()) => {}
    }
    if options.dirs.is_empty() {
        options.dirs.push(PathBuf::from("."));
    }
    let report = match check(&options) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("tilo-layout-check: {error}");
            return ExitCode::from(2);
        }
    };
    if json {
        match serde_json::to_string_pretty(&report) {
            Ok(text) => println!("{text}"),
            Err(error) => {
                eprintln!("tilo-layout-check: {error}");
                return ExitCode::from(2);
            }
        }
    } else {
        print!("{}", render(&report));
    }
    if report.new_findings().next().is_some()
        || (strict && !(report.stale.is_empty() && report.unreached.is_empty()))
    {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn value(args: &mut std::slice::Iter<'_, String>, flag: &str) -> Result<String, String> {
    args.next()
        .cloned()
        .ok_or_else(|| format!("{flag} needs a value"))
}
