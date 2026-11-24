//! `tilolang layout-check`'s command line: its options read into [`Options`], the check run, its
//! report written, and the exit it ends with. A program that adds to what layouts stand on (sample
//! data of an outside service, registered with `tilolang_tilo::origin::register_sample`) runs the
//! same command through [`run`].

use std::path::PathBuf;
use std::process::ExitCode;

use super::{Dump, Options, check, render};

/// Run `tilolang layout-check` with `args` (what follows `layout-check`): 0 when the layouts
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
                        "unknown option {flag}; `tilolang layout-check --help` lists the options"
                    ));
                }
                dir => options.dirs.push(dir.into()),
            }
        }
        Ok(())
    })();
    if let Err(error) = parsed {
        eprintln!("tilolang: {error}");
        return ExitCode::from(2);
    }
    if options.dirs.is_empty() {
        options.dirs.push(PathBuf::from("."));
    }
    let report = match check(&options) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("tilolang layout-check: {error}");
            return ExitCode::from(2);
        }
    };
    if json {
        match serde_json::to_string_pretty(&report) {
            Ok(text) => println!("{text}"),
            Err(error) => {
                eprintln!("tilolang layout-check: {error}");
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
