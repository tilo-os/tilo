//! An app's committed public surface: `surface.toml`.
//!
//! The app's own binary reports what its resource publishes (its verbs, their
//! parameters and kinds, its stored fields) and which of its view's controls
//! are bound to something the resource does not publish
//! (`ui::framework::testing::surface_report`, run as `<app> --surface`).
//! `tilo-app surface` commits the surface next to the manifest, where review
//! sees every rename and the OS reads what an app that is not running
//! offers. `tilo-app check` and CI compare the committed file with the code:
//! a difference, an incompatible storage change without a version bump, or a
//! control bound to nothing fails; a field or verb without a history message
//! in `locales/en-US.ftl` is reported.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use anyhow::{Context as _, Result, bail};
use serde_json::Value;

use crate::contract::{ContractFinding, ContractReport, Level};

/// The committed surface's file name, at the app's root.
pub const FILE: &str = "surface.toml";

/// The flag that makes a framework app's binary print its report.
pub const FLAG: &str = "--surface";

const HEADER: &str = "# The app's public surface, written by `tilo-app surface`: its resources' verbs\n# and stored fields. Review changes to it like an API change; `tilo-app check`\n# fails when the code and this file disagree.\n\n";

/// What an app's binary reports about its surface.
#[derive(Debug, Default)]
pub struct SurfaceReport {
    /// One surface per resource kind, as JSON.
    pub resources: Vec<Value>,
    /// Controls bound to something the resource does not publish.
    pub unbound: Vec<String>,
    /// Why the view could not be built headless, if it could not.
    pub view: Option<String>,
    /// The ids of the effects the view publishes.
    pub effects: Vec<String>,
    /// The ids of the resource's named views.
    pub views: Vec<String>,
    /// The ids the framework publishes its own actions under, as the app's
    /// framework reports them.
    pub reserved: Vec<String>,
}

/// Whether the app at `app_dir` runs through the framework, whose binary
/// answers [`FLAG`]: its entry point is `ui::main!(App)` or
/// `framework::run::<App>()`, not a component definition.
#[must_use]
pub fn is_framework_app(app_dir: &Path) -> bool {
    let Ok(main) = std::fs::read_to_string(app_dir.join("src/main.rs")) else {
        return false;
    };
    let main: String = main.split_whitespace().collect();
    (main.contains("ui::main!(") && !main.contains("ui::main!(definition"))
        || main.contains("framework::run::<")
}

/// Parse what an app's binary printed for [`FLAG`].
///
/// # Errors
/// Returns why the output is not a surface report.
pub fn parse_report(output: &str) -> Result<SurfaceReport> {
    let start = output
        .find('{')
        .context("the app printed no surface report")?;
    let report: Value = serde_json::from_str(output.get(start..).unwrap_or_default().trim())
        .context("the app's surface report is not JSON")?;
    let resources = report
        .get("resources")
        .and_then(Value::as_array)
        .context("the surface report names no resources")?
        .iter()
        .map(normalized)
        .collect();
    let strings = |key: &str| -> Vec<String> {
        report
            .get(key)
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    let unbound = strings("unbound");
    let view = report
        .get("view")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(SurfaceReport {
        resources,
        unbound,
        view,
        effects: strings("effects"),
        views: strings("views"),
        reserved: strings("reserved"),
    })
}

/// Build and run the app's binary for its surface report.
///
/// # Errors
/// Returns why the app does not build or report.
pub fn read_report(app_dir: &Path) -> Result<SurfaceReport> {
    if !is_framework_app(app_dir) {
        bail!(
            "{} does not run through `ui::main!(App)`, so its binary reports no surface",
            app_dir.display()
        );
    }
    let program = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let output = ProcessCommand::new(&program)
        .current_dir(app_dir)
        .args(["run", "--quiet", "--manifest-path"])
        .arg(app_dir.join("Cargo.toml"))
        .args(["--", FLAG])
        .stderr(std::process::Stdio::inherit())
        .output()
        .with_context(|| format!("starting {}", program.to_string_lossy()))?;
    if !output.status.success() {
        bail!("the app's surface report failed with {}", output.status);
    }
    parse_report(&String::from_utf8_lossy(&output.stdout))
}

/// Run an already built `executable` of the app for its surface report.
///
/// # Errors
/// Returns why it did not report.
pub fn report_of(executable: &Path, app_dir: &Path) -> Result<SurfaceReport> {
    let output = ProcessCommand::new(executable)
        .current_dir(app_dir)
        .arg(FLAG)
        .stderr(std::process::Stdio::inherit())
        .output()
        .with_context(|| format!("running {} {FLAG}", executable.display()))?;
    if !output.status.success() {
        bail!(
            "{} {FLAG} failed with {}",
            executable.display(),
            output.status
        );
    }
    parse_report(&String::from_utf8_lossy(&output.stdout))
}

/// `value` without JSON nulls, which TOML cannot hold and which mean "not
/// there" in a surface.
fn normalized(value: &Value) -> Value {
    match value {
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .filter(|(_, value)| !value.is_null())
                .map(|(key, value)| (key.clone(), normalized(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .filter(|item| !item.is_null())
                .map(normalized)
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The surface file's text for `resources`.
///
/// # Errors
/// Returns why the surface cannot be written as TOML.
pub fn render(resources: &[Value]) -> Result<String> {
    let document =
        serde_json::json!({ "resources": resources.iter().map(normalized).collect::<Vec<_>>() });
    let toml = toml::Value::try_from(&document).context("the surface does not fit TOML")?;
    let mut text = String::from(HEADER);
    text.push_str(&toml::to_string_pretty(&toml).context("writing the surface as TOML")?);
    Ok(text)
}

/// Write `resources` to the app's [`FILE`]. Returns its path.
///
/// # Errors
/// Returns why it could not be written.
pub fn write(app_dir: &Path, resources: &[Value]) -> Result<PathBuf> {
    let path = app_dir.join(FILE);
    std::fs::write(&path, render(resources)?)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// The committed surface, if the app has one.
///
/// # Errors
/// Returns why the committed file does not read.
pub fn committed(app_dir: &Path) -> Result<Option<Vec<Value>>> {
    let path = app_dir.join(FILE);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let toml: toml::Value =
        toml::from_str(&text).with_context(|| format!("{} is not TOML", path.display()))?;
    let json = serde_json::to_value(toml).context("reading the committed surface")?;
    Ok(Some(
        json.get("resources")
            .and_then(Value::as_array)
            .map(|resources| resources.iter().map(normalized).collect())
            .unwrap_or_default(),
    ))
}

fn keyed<'a>(items: impl IntoIterator<Item = &'a Value>, key: &str) -> BTreeMap<String, &'a Value> {
    items
        .into_iter()
        .filter_map(|item| {
            item.get(key)
                .and_then(Value::as_str)
                .map(|name| (name.to_owned(), item))
        })
        .collect()
}

fn by<'a>(items: Option<&'a Value>, key: &str) -> BTreeMap<String, &'a Value> {
    keyed(items.and_then(Value::as_array).into_iter().flatten(), key)
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// How the committed surface `before` differs from `now`, one line each.
fn differences(kind: &str, before: &Value, now: &Value) -> Vec<String> {
    let mut changes = Vec::new();
    for (what, list, key) in [("verb", "verbs", "id"), ("field", "fields", "path")] {
        let old = by(before.get(list), key);
        let new = by(now.get(list), key);
        for (name, value) in &new {
            match old.get(name) {
                None => changes.push(format!("{kind}: new {what} `{name}`")),
                Some(previous) if previous != value => {
                    changes.push(format!("{kind}: {what} `{name}` changed"));
                }
                Some(_) => {}
            }
        }
        for name in old.keys().filter(|name| !new.contains_key(*name)) {
            changes.push(format!("{kind}: {what} `{name}` is gone"));
        }
    }
    for key in ["label", "version"] {
        if before.get(key) != now.get(key) {
            changes.push(format!("{kind}: {key} changed"));
        }
    }
    changes
}

/// The stored fields of `before` that `now` reads differently (gone, or
/// another type or layout), which older documents need a migration for.
fn incompatible(before: &Value, now: &Value) -> Vec<String> {
    let old = by(before.get("fields"), "path");
    let new = by(now.get("fields"), "path");
    old.iter()
        .filter_map(|(path, field)| match new.get(path) {
            None => Some(format!("`{path}` is gone")),
            Some(current)
                if text(current, "type") != text(field, "type")
                    || text(current, "stored") != text(field, "stored") =>
            {
                Some(format!(
                    "`{path}` is now {} ({}), was {} ({})",
                    text(current, "type"),
                    text(current, "stored"),
                    text(field, "type"),
                    text(field, "stored")
                ))
            }
            Some(_) => None,
        })
        .collect()
}

/// The history messages `resource` asks of `locales/en-US.ftl`, as history
/// renders entries of the app `app` (its id): one per verb that changes
/// something (`<app>-<verb>`) and one per editable field
/// (`<app>-<type>-<field>`).
fn history_messages(app: &str, resource: &Value) -> Vec<String> {
    let app = tilo_history::app_prefix(app);
    let kebab = tilo_actions::entry::kebab;
    let mut keys = Vec::new();
    for verb in resource
        .get("verbs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if text(verb, "kind") != "read" {
            keys.push(format!("{app}-{}", kebab(text(verb, "id"))));
        }
    }
    let fields: Vec<&Value> = resource
        .get("fields")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect();
    let readonly = |field: &Value| {
        field
            .get("readonly")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    // A field inside a read-only one is changed only by verbs too.
    let locked: Vec<String> = fields
        .iter()
        .filter(|field| readonly(field))
        .map(|field| format!("{}.", text(field, "path")))
        .collect();
    for field in fields {
        let path = text(field, "path");
        let editable = text(field, "stored") == "value"
            && !readonly(field)
            && !locked
                .iter()
                .any(|prefix| path.starts_with(prefix.as_str()));
        if editable {
            let key = text(field, "path").rsplit('.').next().unwrap_or_default();
            keys.push(format!(
                "{app}-{}-{}",
                kebab(text(field, "owner")),
                kebab(key)
            ));
        }
    }
    keys
}

/// What `tilo-app check` reports about the app's surface: unbound controls,
/// drift from the committed file, incompatible storage changes without a
/// version bump, and missing history messages. A missing committed file is
/// `missing`'s level.
#[must_use]
pub fn findings(
    app: &str,
    report: &SurfaceReport,
    committed: Option<&[Value]>,
    catalog: Option<&BTreeSet<String>>,
    missing: Level,
) -> ContractReport {
    let file = PathBuf::from(FILE);
    let finding = |level, rule, message: String| ContractFinding {
        path: file.clone(),
        line: 0,
        level,
        rule,
        message,
    };
    let mut out = ContractReport::default();
    if let Some(view) = &report.view {
        out.findings.push(finding(
            Level::Error,
            "view",
            format!("the view could not be built headless: {view}"),
        ));
    }
    for unbound in &report.unbound {
        out.findings
            .push(finding(Level::Error, "binding", unbound.clone()));
    }
    for (what, id) in own_action_ids(report) {
        if report.reserved.contains(&id) {
            out.findings.push(finding(
                Level::Error,
                "reserved-id",
                format!(
                    "{what} `{id}` has the id of an action the framework publishes on every app; \
                     rename it (the framework's ids: {})",
                    report.reserved.join(", ")
                ),
            ));
        }
    }
    match committed {
        None => out.findings.push(finding(
            missing,
            "surface",
            format!("there is no committed {FILE}; write it with `tilo-app surface`"),
        )),
        Some(committed) => {
            let old = keyed(committed, "kind");
            let new = keyed(&report.resources, "kind");
            let mut changes = Vec::new();
            for (kind, now) in &new {
                let Some(before) = old.get(kind) else {
                    changes.push(format!("new resource `{kind}`"));
                    continue;
                };
                changes.extend(differences(kind, before, now));
                let storage = incompatible(before, now);
                let bumped = now.get("version").and_then(Value::as_u64)
                    > before.get("version").and_then(Value::as_u64);
                if !storage.is_empty() && !bumped {
                    out.findings.push(finding(
                        Level::Error,
                        "storage",
                        format!(
                            "{kind} reads stored fields differently ({}) without a new storage version; bump `#[resource(version = N)]` and migrate older documents",
                            storage.join(", ")
                        ),
                    ));
                }
            }
            for kind in old.keys().filter(|kind| !new.contains_key(*kind)) {
                changes.push(format!("resource `{kind}` is gone"));
            }
            if !changes.is_empty() {
                out.findings.push(finding(
                    Level::Error,
                    "surface",
                    format!(
                        "the code's surface differs from {FILE} ({}); review it and write it with `tilo-app surface`",
                        changes.join("; ")
                    ),
                ));
            }
        }
    }
    if let Some(catalog) = catalog {
        let missing: Vec<String> = report
            .resources
            .iter()
            .flat_map(|resource| history_messages(app, resource))
            .filter(|key| !catalog.contains(key))
            .collect();
        if !missing.is_empty() {
            let mut message = String::from(
                "history falls back to generic sentences without these messages in locales/en-US.ftl: ",
            );
            let _ = write!(message, "{}", missing.join(", "));
            out.findings.push(ContractFinding {
                path: PathBuf::from("locales/en-US.ftl"),
                ..finding(Level::Warning, "history-message", message)
            });
        }
    }
    out
}

/// Every id the app publishes an action or view under itself: its
/// resources' verbs, its effects and its named views, each with what it is.
fn own_action_ids(report: &SurfaceReport) -> Vec<(&'static str, String)> {
    let verbs = report.resources.iter().flat_map(|resource| {
        resource
            .get("verbs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|verb| ("verb", text(verb, "id").to_owned()))
    });
    let effects = report.effects.iter().map(|id| ("effect", id.clone()));
    let views = report.views.iter().map(|id| ("view", id.clone()));
    verbs.chain(effects).chain(views).collect()
}

/// Write the surface of the app containing `start` to its [`FILE`], from
/// its binary's report. Returns the file and the report, whose unbound
/// controls the caller shows.
///
/// # Errors
/// Returns why the app does not report or the file cannot be written.
pub fn write_app(start: &Path) -> Result<(PathBuf, SurfaceReport)> {
    let app_dir = crate::resolve_app_dir(start)?;
    let report = read_report(&app_dir)?;
    let path = write(&app_dir, &report.resources)?;
    Ok((path, report))
}

/// The app id `tilo-app.toml` declares.
///
/// # Errors
/// Returns why the manifest does not read.
pub fn app_id(app_dir: &Path) -> Result<String> {
    let path = app_dir.join("tilo-app.toml");
    let source =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let manifest = tilo_app_package::Manifest::from_toml_str(&source)
        .with_context(|| format!("parsing {}", path.display()))?;
    Ok(manifest.package.id)
}

/// Write the surface of the app containing `start` from the report its
/// binary printed for [`FLAG`], saved at `report`.
///
/// # Errors
/// Returns why the report does not read or the file cannot be written.
pub fn write_reported(start: &Path, report: &Path) -> Result<(PathBuf, SurfaceReport)> {
    let app_dir = crate::resolve_app_dir(start)?;
    let text =
        std::fs::read_to_string(report).with_context(|| format!("reading {}", report.display()))?;
    let report = parse_report(&text)?;
    let path = write(&app_dir, &report.resources)?;
    Ok((path, report))
}

/// Check the surface of the app containing `start` against its committed
/// [`FILE`]; `None` for an app whose binary reports none.
///
/// # Errors
/// Returns why the app does not report or the files do not read.
pub fn check_app(start: &Path) -> Result<Option<ContractReport>> {
    let app_dir = crate::resolve_app_dir(start)?;
    if !is_framework_app(&app_dir) {
        return Ok(None);
    }
    let report = read_report(&app_dir)?;
    let catalog = crate::contract::source_catalog(&app_dir)?;
    Ok(Some(findings(
        &app_id(&app_dir)?,
        &report,
        committed(&app_dir)?.as_deref(),
        catalog.as_ref(),
        Level::Error,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resource(version: u64, title_type: &str, verbs: &[&str]) -> Value {
        serde_json::json!({
            "kind": "notes.book",
            "label": "Notes",
            "version": version,
            "verbs": verbs.iter().map(|id| serde_json::json!({
                "id": id, "label": id, "kind": "undoable", "params": [
                    {"name": "title", "required": true, "schema": {"type": "string", "default": null}}
                ]
            })).collect::<Vec<_>>(),
            "fields": [
                {"path": "title", "owner": "book", "title": "Title", "stored": "value", "type": title_type},
                {"path": "pages", "owner": "book", "title": "Pages", "stored": "collection", "type": "Collection<Page>"},
                {"path": "pages.*.text", "owner": "page", "title": "Text", "stored": "value", "type": "String", "readonly": true},
                {"path": "meta", "owner": "book", "title": "Meta", "stored": "record", "type": "Meta", "readonly": true},
                {"path": "meta.author", "owner": "meta", "title": "Author", "stored": "value", "type": "String"},
            ],
        })
    }

    fn report(resources: &[Value]) -> SurfaceReport {
        SurfaceReport {
            resources: resources.iter().map(normalized).collect(),
            unbound: Vec::new(),
            view: None,
            effects: Vec::new(),
            views: Vec::new(),
            reserved: Vec::new(),
        }
    }

    fn rules(found: &ContractReport) -> Vec<(&'static str, Level)> {
        found
            .findings
            .iter()
            .map(|finding| (finding.rule, finding.level))
            .collect()
    }

    #[test]
    fn a_written_surface_reads_back_equal_and_passes() {
        let temp = tempfile::tempdir().unwrap();
        let now = report(&[resource(1, "String", &["rename"])]);
        write(temp.path(), &now.resources).unwrap();
        let committed = committed(temp.path()).unwrap().unwrap();
        assert_eq!(committed, now.resources);
        let catalog: BTreeSet<String> = ["notes-rename", "notes-book-title"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let found = findings(
            "so.tilo.notes",
            &now,
            Some(&committed),
            Some(&catalog),
            Level::Error,
        );
        assert!(found.findings.is_empty(), "{}", found.render());
    }

    #[test]
    fn drift_storage_changes_bindings_and_messages_are_reported() {
        let before = vec![normalized(&resource(1, "String", &["rename"]))];
        // A renamed verb is drift; a field read as another type is a
        // storage change that needs a version.
        let now = report(&[resource(1, "Atom", &["retitle"])]);
        let found = findings(
            "so.tilo.notes",
            &now,
            Some(&before),
            Some(&BTreeSet::new()),
            Level::Error,
        );
        assert_eq!(
            rules(&found),
            [
                ("storage", Level::Error),
                ("surface", Level::Error),
                ("history-message", Level::Warning)
            ]
        );
        assert!(found.findings[1].message.contains("verb `retitle`"));
        assert!(found.findings[2].message.contains("notes-retitle"));
        assert!(found.findings[2].message.contains("notes-book-title"));
        assert!(!found.findings[2].message.contains("notes-page-text"));
        assert!(!found.findings[2].message.contains("notes-meta-author"));

        // With a new storage version the change is drift to commit only.
        let bumped = report(&[resource(2, "Atom", &["rename"])]);
        let found = findings("so.tilo.notes", &bumped, Some(&before), None, Level::Error);
        assert_eq!(rules(&found), [("surface", Level::Error)]);

        let mut unbound = report(&[resource(1, "String", &["rename"])]);
        unbound.unbound.push("a control is bound to `x`".into());
        let found = findings("so.tilo.notes", &unbound, None, None, Level::Warning);
        assert_eq!(
            rules(&found),
            [("binding", Level::Error), ("surface", Level::Warning)]
        );
        assert!(!found.passed());
    }

    #[test]
    fn the_report_is_read_from_what_the_binary_printed() {
        let printed = "starting\n{\"resources\": [{\"kind\": \"notes.book\", \"verbs\": [], \"fields\": []}], \"unbound\": [\"x\"], \"effects\": [\"zoom-in\"], \"views\": [\"reader\"], \"reserved\": [\"open\", \"undo\"], \"view\": null}\n";
        let parsed = parse_report(printed).unwrap();
        assert_eq!(parsed.resources.len(), 1);
        assert_eq!(parsed.unbound, ["x"]);
        assert_eq!(parsed.effects, ["zoom-in"]);
        assert_eq!(parsed.views, ["reader"]);
        assert_eq!(parsed.reserved, ["open", "undo"]);
        assert!(parsed.view.is_none());
    }

    /// A verb, effect or view under an id the framework publishes its own
    /// action under is refused, whichever it is; other ids pass.
    #[test]
    fn an_action_with_a_framework_id_is_refused() {
        let mut now = report(&[resource(1, "String", &["rename", "undo"])]);
        now.effects = vec!["zoom-in".into(), "open".into()];
        now.views = vec!["history".into(), "reader".into()];
        now.reserved = ["open", "undo", "redo", "history", "field-edit"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let found = findings("so.tilo.notes", &now, None, None, Level::Warning);
        let refused: Vec<&str> = found
            .findings
            .iter()
            .filter(|finding| finding.rule == "reserved-id")
            .map(|finding| finding.message.as_str())
            .collect();
        assert_eq!(refused.len(), 3, "{refused:?}");
        assert!(refused[0].starts_with("verb `undo`"));
        assert!(refused[1].starts_with("effect `open`"));
        assert!(refused[2].starts_with("view `history`"));
        assert!(!found.passed());
    }

    #[test]
    fn only_framework_apps_report_a_surface() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("src/main.rs"), "ui::main!(demo::App);\n").unwrap();
        assert!(is_framework_app(temp.path()));
        std::fs::write(
            temp.path().join("src/main.rs"),
            "ui::main!(definition = demo::definition);\n",
        )
        .unwrap();
        assert!(!is_framework_app(temp.path()));
    }
}
