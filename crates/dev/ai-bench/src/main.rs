//! The benchmark's command line.
//!
//! One run = one or more suites asked of one or more endpoints. It exits
//! non-zero when a case failed, so CI can gate on it, and prints the cost of
//! the run beside the score, so a hosted endpoint's bill is never a surprise.

#![deny(missing_docs)]

use std::path::PathBuf;
use std::process::ExitCode;

use tilo_ai_bench::case::Suite;
use tilo_ai_bench::endpoint::{self, Endpoint, Endpoints};
use tilo_ai_bench::report::Report;
use tilo_ai_bench::run;

const USAGE: &str = "\
tilo-ai-bench --suite FILE... --endpoints FILE --endpoint LABEL... [options]

  --suite FILE        a suite to run; repeat for several
  --endpoints FILE    the endpoint list (labels, URLs, models, prices)
  --endpoint LABEL    which endpoint of that list to run; repeat for several
  --key-env NAME      the variable holding that endpoint's key, in the same
                      order as --endpoint; a local endpoint takes none
  --out FILE          write the report's JSON here instead of stdout
  --quiet             print only failures
";

/// What the command was asked to do.
struct Options {
    suites: Vec<PathBuf>,
    endpoints_file: PathBuf,
    endpoints: Vec<String>,
    key_names: Vec<String>,
    out: Option<PathBuf>,
    quiet: bool,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let options = match parse(&args) {
        Ok(options) => options,
        Err(why) => {
            eprintln!("tilo-ai-bench: {why}");
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run_options(&options) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(why) => {
            eprintln!("tilo-ai-bench: {why}");
            ExitCode::from(2)
        }
    }
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut suites = Vec::new();
    let mut endpoints_file = None;
    let mut endpoints = Vec::new();
    let mut key_names = Vec::new();
    let mut out = None;
    let mut quiet = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--suite" => suites.push(PathBuf::from(next(&mut args, "--suite")?)),
            "--endpoints" => endpoints_file = Some(PathBuf::from(next(&mut args, "--endpoints")?)),
            "--endpoint" => endpoints.push(next(&mut args, "--endpoint")?),
            "--key-env" => key_names.push(next(&mut args, "--key-env")?),
            "--out" => out = Some(PathBuf::from(next(&mut args, "--out")?)),
            "--quiet" => quiet = true,
            other => return Err(format!("`{other}` is not an option this tool takes")),
        }
    }
    let endpoints_file = endpoints_file.ok_or("a run needs an --endpoints file")?;
    if suites.is_empty() {
        return Err("a run needs at least one --suite".into());
    }
    if endpoints.is_empty() {
        return Err("a run needs at least one --endpoint".into());
    }
    if key_names.len() > endpoints.len() {
        return Err("there are more --key-env than --endpoint".into());
    }
    Ok(Options {
        suites,
        endpoints_file,
        endpoints,
        key_names,
        out,
        quiet,
    })
}

fn next(args: &mut std::slice::Iter<'_, String>, option: &str) -> Result<String, String> {
    args.next()
        .cloned()
        .ok_or_else(|| format!("{option} needs a value"))
}

fn run_options(options: &Options) -> Result<bool, String> {
    let named = Endpoints::load(&options.endpoints_file)?;
    let mut chosen = Vec::new();
    for (index, label) in options.endpoints.iter().enumerate() {
        let endpoint = named.get(label).ok_or_else(|| {
            let offered = named.labels().join(", ");
            format!(
                "{} names no endpoint `{label}`, only {offered}",
                options.endpoints_file.display()
            )
        })?;
        let key = match options.key_names.get(index) {
            Some(name) => std::env::var(name)
                .map_err(|_| format!("{label} needs its key in ${name}, and {name} is not set"))?,
            None => String::new(),
        };
        chosen.push((endpoint.clone(), key));
    }
    let client = endpoint::client()?;
    let mut runs = Vec::new();
    for path in &options.suites {
        let suite = Suite::load(path).map_err(|error| error.to_string())?;
        eprintln!("{}: {} cases", path.display(), suite.cases.len());
        let named: Vec<(&Endpoint, String)> = chosen
            .iter()
            .map(|(endpoint, key)| (endpoint, key.clone()))
            .collect();
        let report = run::run_all(&client, &named, &suite, |case| {
            if options.quiet && case.passed() {
                return;
            }
            let verdict = if case.passed() { "ok" } else { "failed" };
            eprintln!("  {verdict} {}", case.case);
        });
        runs.extend(report.runs);
    }
    let report = Report { runs };
    match &options.out {
        Some(path) => std::fs::write(path, report.to_json()?)
            .map_err(|error| format!("{} cannot be written: {error}", path.display()))?,
        None => print!("{}", report.to_text()),
    }
    Ok(report.passed())
}
