//! `tilolang surfaces`: every committed surface against its code.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use anyhow::{Context as _, Result};
use serde_json::{Value, json};

use super::is_windowless_service;

#[derive(clap::Args)]
pub(crate) struct SurfacesArgs {
    /// The workspace root holding `apps/`. Defaults to the current directory.
    path: Option<PathBuf>,

    /// Write each app's surface instead of comparing it.
    #[arg(long)]
    write: bool,

    /// Keep each app's surface report in this directory, as
    /// `<app>.json`, instead of comparing it: what `surface --report`
    /// writes a surface from without building the app again.
    #[arg(long)]
    reports: Option<PathBuf>,
}

pub(crate) fn workspace_apps(path: &Path) -> Result<Vec<(PathBuf, String, String)>> {
    let apps = path.join("apps");
    anyhow::ensure!(
        apps.is_dir(),
        "{} is not a Tilo workspace root",
        path.display()
    );
    let mut app_dirs = std::fs::read_dir(&apps)?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|dir| dir.join("Cargo.toml").is_file())
        // An application has a manifest; a crate beside the apps without one
        // (the Browser's parts, Settings' pane libraries) is a library an
        // application links, certified through that application.
        .filter(|dir| dir.join("tilo-app.toml").is_file())
        // The agent is the headless consumer being used to drive apps; its
        // broker protocol is certified by the structural contract and tests.
        .filter(|dir| dir.file_name().is_none_or(|name| name != "agent"))
        // An `apps/<name>-model` crate is an app's shared data library, not
        // an application.
        .filter(|dir| {
            dir.file_name()
                .is_none_or(|name| !name.to_string_lossy().ends_with("-model"))
        })
        // A windowless service has no UI to audit: a compute package's
        // worker runs its `[cli]` command for another app's action, which
        // that app certifies, and a service such as web search only
        // publishes actions, which its own tests cover.
        .filter(|dir| !is_windowless_service(dir))
        .collect::<Vec<_>>();
    app_dirs.sort();
    let mut packages = Vec::with_capacity(app_dirs.len());
    for app in &app_dirs {
        let manifest_path = app.join("Cargo.toml");
        let manifest = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("reading {}", manifest_path.display()))?
            .parse::<toml::Table>()
            .with_context(|| format!("parsing {}", manifest_path.display()))?;
        let package = manifest
            .get("package")
            .and_then(toml::Value::as_table)
            .with_context(|| format!("{} has no [package] table", manifest_path.display()))?;
        let package_name = package
            .get("name")
            .and_then(toml::Value::as_str)
            .with_context(|| format!("{} has no package name", manifest_path.display()))?;
        let explicit_bins = manifest
            .get("bin")
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|binary| binary.get("name").and_then(toml::Value::as_str))
            .collect::<Vec<_>>();
        let binary_name = package
            .get("default-run")
            .and_then(toml::Value::as_str)
            .or(match explicit_bins.as_slice() {
                [binary] => Some(*binary),
                _ => None,
            })
            .unwrap_or(package_name);
        packages.push((app.clone(), package_name.to_owned(), binary_name.to_owned()));
    }
    Ok(packages)
}

/// Build `packages` in one Cargo graph and answer each executable's path by
/// its target name.
pub(crate) fn build_apps(
    path: &Path,
    packages: &[(PathBuf, String, String)],
) -> Result<std::collections::HashMap<String, PathBuf>> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let mut build = ProcessCommand::new(cargo);
    build
        .current_dir(path)
        .arg("build")
        .arg("--manifest-path")
        .arg(path.join("Cargo.toml"))
        .arg("--message-format=json-render-diagnostics");
    for (_, package, _) in packages {
        build.arg("--package").arg(package);
    }
    let answer = tilo_app_builder::surface::run_bounded(
        build,
        tilo_app_builder::surface::BUILD_WAIT,
        "the workspace application build",
    )?;
    anyhow::ensure!(
        answer.status.success(),
        "the workspace application build failed with {}",
        answer.status
    );
    let mut executables = std::collections::HashMap::new();
    for line in String::from_utf8_lossy(&answer.stdout).lines() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if message.get("reason").and_then(serde_json::Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        let Some(executable) = message
            .get("executable")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let Some(name) = message
            .get("target")
            .and_then(|target| target.get("name"))
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        executables.insert(name.to_owned(), PathBuf::from(executable));
    }
    Ok(executables)
}

/// `tilolang surfaces`: every app's committed surface against its code, one
/// build of the whole application set, or those surfaces written again.
pub(crate) fn run_surfaces(args: SurfacesArgs) -> Result<Value> {
    let path = args.path.map_or_else(std::env::current_dir, Ok)?;
    let packages = workspace_apps(&path)?;
    let executables = build_apps(&path, &packages)?;
    let mut written = Vec::new();
    let mut kept = Vec::new();
    let mut failures = Vec::new();
    let reports = args.reports.as_deref().map(|dir| {
        if dir.is_absolute() {
            dir.to_path_buf()
        } else {
            path.join(dir)
        }
    });
    for (app, _, binary) in &packages {
        if !tilo_app_builder::surface::is_framework_app(app) {
            continue;
        }
        let Some(executable) = executables.get(binary) else {
            failures.push(format!(
                "Cargo built no executable named {binary:?} for {}",
                app.display()
            ));
            continue;
        };
        if let Some(dir) = reports.as_deref() {
            let file = tilo_app_builder::surface::keep_report(app, executable, dir)?;
            println!("Kept {}", file.display());
            kept.push(file);
            continue;
        }
        let report = tilo_app_builder::surface::report_of(app, executable)
            .with_context(|| format!("reading {}'s surface", app.display()))?;
        if let Some(view) = &report.view {
            // Its read actions are missing from a report whose view did not
            // build, so it says nothing about the surface the code publishes.
            failures.push(format!(
                "{}: the view could not be built headless: {view}",
                app.display()
            ));
            continue;
        }
        for unbound in &report.unbound {
            println!("warning: {}: {unbound}", app.display());
        }
        if args.write {
            let file = tilo_app_builder::surface::write(app, &report.resources, &report.owners)?;
            println!("Wrote {}", file.display());
            written.push(file);
            if tilo_app_builder::surface::write_offers(app, &report.offers)? {
                println!("Wrote {}'s offers", app.display());
            }
        } else if let Err(error) = tilo_app_builder::surface::compiled_is_current(app) {
            failures.push(format!("{}: {error}", app.display()));
            continue;
        }
        match tilo_app_builder::surface::check_report(
            app,
            &report,
            tilo_app_builder::contract::Level::Error,
        ) {
            Ok(contract) => {
                // What this check is for: the committed surface against the
                // code. A control bound to nothing, an id the framework
                // publishes, or a missing history message is the app
                // contract's own finding (`tilolang check`), which `app-lints`
                // and certification hold; a surface behind its code is what no
                // other per-change check sees.
                let mut contract = contract;
                contract
                    .findings
                    .retain(|finding| matches!(finding.rule, "surface" | "storage"));
                print!("{}", contract.render());
                if !contract.passed() {
                    failures.push(format!("{}:\n{}", app.display(), contract.render()));
                }
            }
            Err(error) => failures.push(format!("{error:#}")),
        }
    }
    if !failures.is_empty() {
        for failure in &failures {
            eprintln!("{failure}");
        }
        return Err(super::failed(
            format!("{} app surface(s) are behind their code", failures.len()),
            json!({"failures": failures}),
        ));
    }
    if reports.is_some() {
        println!("Kept {} surface reports", kept.len());
        return Ok(json!({"apps": packages.len(), "reports": kept}));
    }
    println!("Every app's surface is current");
    Ok(json!({"apps": packages.len(), "written": written}))
}
