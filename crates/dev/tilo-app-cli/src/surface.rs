//! An app's committed public surface: `surface.toml`.
//!
//! The app's own binary reports what its resource publishes (its verbs, their
//! parameters and kinds, its stored fields) and which of its view's controls
//! are bound to something the resource does not publish
//! (`ui::framework::headless::surface_report`, run as `<app> --surface`).
//! `tilo-app surface` commits the surface next to the manifest, where review
//! sees every rename and the OS reads what an app that is not running
//! offers. `tilo-app check` and CI compare the committed file with the code:
//! a difference, an incompatible storage change without a version bump, or a
//! control bound to nothing fails; a field or verb without a history message
//! in `locales/en-US.ftl` is reported.
//!
//! The file is the bytes a package ships and a change names by hash, so it
//! keeps only what deciding a change and checking storage need ([`filed`]):
//! kinds, verb ids, kinds, footprints and parameters, field paths, layouts
//! and types. Labels, descriptions, field titles and owners stay in the
//! code; a field type's own operation vocabulary is kept as its digest. One
//! line per field keeps the file small and its diffs readable.

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

const HEADER: &str = "# Written by `tilo-app surface`; review changes like an API change.\n\n";

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

/// Run the app's binary for its surface report: `executable` when it is
/// already built, else `cargo run`.
///
/// # Errors
/// Returns why the app does not build or report.
pub fn report_of(app_dir: &Path, executable: Option<&Path>) -> Result<SurfaceReport> {
    let mut command = if let Some(executable) = executable {
        ProcessCommand::new(executable)
    } else {
        let program = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
        let mut cargo = ProcessCommand::new(program);
        cargo
            .args(["run", "--quiet", "--manifest-path"])
            .arg(app_dir.join("Cargo.toml"))
            .arg("--");
        cargo
    };
    let output = command
        .current_dir(app_dir)
        .arg(FLAG)
        .stderr(std::process::Stdio::inherit())
        .output()
        .with_context(|| format!("running {command:?}"))?;
    if !output.status.success() {
        bail!("{command:?} failed with {}", output.status);
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

/// What the surface file keeps of a resource the code reports: see the
/// module docs. Applying it twice changes nothing, so a committed file
/// compares with the code through it too.
#[must_use]
pub fn filed(resource: &Value) -> Value {
    let mut resource = normalized(resource);
    let Some(map) = resource.as_object_mut() else {
        return resource;
    };
    for list in ["verbs", "fields"] {
        if map
            .get(list)
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        {
            map.remove(list);
        }
    }
    for verb in map
        .get_mut("verbs")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object_mut)
    {
        verb.remove("label");
        verb.remove("description");
        if verb
            .get("params")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        {
            verb.remove("params");
        }
        for schema in verb
            .get_mut("params")
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
            .filter_map(|param| param.get_mut("schema"))
        {
            without_prose(schema);
        }
    }
    for field in map
        .get_mut("fields")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object_mut)
    {
        field.remove("title");
        field.remove("owner");
        if field.get("stored").and_then(Value::as_str) == Some("value") {
            field.remove("stored");
        }
        if let Some(ops) = field.get_mut("ops")
            && !ops.is_string()
        {
            *ops = Value::String(digest(ops));
        }
    }
    resource
}

/// `schema` without the prose its types' doc comments put in it
/// (`description`, `examples`): what a parameter accepts stays.
fn without_prose(schema: &mut Value) {
    match schema {
        Value::Object(node) => {
            node.remove("description");
            node.remove("examples");
            for (key, value) in node.iter_mut() {
                match key.as_str() {
                    // Maps from names to schemas: the names are not keywords.
                    "properties" | "$defs" | "definitions" => {
                        for item in value
                            .as_object_mut()
                            .into_iter()
                            .flat_map(|map| map.values_mut())
                        {
                            without_prose(item);
                        }
                    }
                    "const" | "enum" | "default" => {}
                    _ => without_prose(value),
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(without_prose),
        _ => {}
    }
}

/// A short, stable digest of `value`'s canonical JSON.
fn digest(value: &Value) -> String {
    use sha2::Digest as _;
    let canonical = serde_json::to_vec(&canonical(value)).unwrap_or_default();
    let hash = sha2::Sha256::digest(&canonical);
    format!("sha256:{}", hex::encode(hash.get(..8).unwrap_or_default()))
}

/// `value` with every object's keys in order.
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<&String, Value> = map
                .iter()
                .map(|(key, item)| (key, canonical(item)))
                .collect();
            Value::Object(
                sorted
                    .into_iter()
                    .map(|(key, item)| (key.clone(), item))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

/// `key` as a TOML key: bare when it can be, else quoted.
fn toml_key(key: &str) -> String {
    let bare = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if bare {
        key.to_owned()
    } else {
        Value::String(key.to_owned()).to_string()
    }
}

/// `value` as one line of TOML: strings in JSON's escapes (which TOML's
/// basic strings share), objects as inline tables.
fn inline(value: &Value, out: &mut String) -> Result<()> {
    match value {
        Value::Null => bail!("TOML holds no null"),
        Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            out.push_str(&value.to_string());
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                inline(item, out)?;
            }
            out.push(']');
        }
        Value::Object(map) if map.is_empty() => out.push_str("{}"),
        Value::Object(map) => {
            out.push_str("{ ");
            for (index, (key, item)) in leading(map, &["path", "name", "type"]).enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                let _ = write!(out, "{} = ", toml_key(key));
                inline(item, out)?;
            }
            out.push_str(" }");
        }
    }
    Ok(())
}

/// `map`'s entries in the file's order, whatever order they were read in:
/// those named in `first` in that order, then the rest by key.
fn leading<'a>(
    map: &'a serde_json::Map<String, Value>,
    first: &[&str],
) -> impl Iterator<Item = (&'a String, &'a Value)> {
    let mut entries: Vec<(&'a String, &'a Value)> = map.iter().collect();
    entries.sort_by_key(|(key, _)| {
        let rank = first
            .iter()
            .position(|lead| *lead == key.as_str())
            .unwrap_or(first.len());
        (rank, key.as_str())
    });
    entries.into_iter()
}

/// The surface file's text for `resources`: each resource's scalars, its
/// fields one per line, then its verbs, their parameters one per line.
///
/// # Errors
/// Returns why the surface cannot be written as TOML.
pub fn render(resources: &[Value]) -> Result<String> {
    let mut text = String::from(HEADER);
    for resource in resources.iter().map(filed) {
        let Value::Object(map) = &resource else {
            bail!("a resource of the surface is not a table");
        };
        text.push_str("[[resources]]\n");
        for (key, value) in leading(map, &["kind", "label", "version", "person"])
            .filter(|(key, _)| !matches!(key.as_str(), "verbs" | "fields"))
        {
            let _ = write!(text, "{} = ", toml_key(key));
            inline(value, &mut text)?;
            text.push('\n');
        }
        if let Some(fields) = map.get("fields").and_then(Value::as_array) {
            text.push_str("fields = [\n");
            for field in fields {
                text.push_str("  ");
                inline(field, &mut text)?;
                text.push_str(",\n");
            }
            text.push_str("]\n");
        }
        for verb in map
            .get("verbs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Value::Object(verb) = verb else {
                bail!("a verb of the surface is not a table");
            };
            text.push_str("\n[[resources.verbs]]\n");
            let params = verb.iter().filter(|(key, _)| key.as_str() == "params");
            let rest = leading(verb, &["id"]).filter(|(key, _)| key.as_str() != "params");
            for (key, value) in rest.chain(params) {
                let _ = write!(text, "{} = ", toml_key(key));
                match value.as_array() {
                    Some(items) if key == "params" && !items.is_empty() => {
                        text.push_str("[\n");
                        for item in items {
                            text.push_str("  ");
                            inline(item, &mut text)?;
                            text.push_str(",\n");
                        }
                        text.push(']');
                    }
                    _ => inline(value, &mut text)?,
                }
                text.push('\n');
            }
        }
        text.push('\n');
    }
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
                if stored_type(text(current, "type")) != stored_type(text(field, "type"))
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

/// A field's type as the document stores it: a shared `Map<T>` is stored
/// as a `BTreeMap<String, T>` is, so moving a field between the two needs
/// no migration.
fn stored_type(ty: &str) -> String {
    let ty = ty
        .replace("ui::tilo_resource::", "")
        .replace("tilo_resource::", "")
        .replace("std::collections::", "");
    let mut out = String::with_capacity(ty.len());
    let mut rest = ty.as_str();
    while let Some(at) = rest.find("Map<") {
        let (before, after) = rest.split_at(at);
        out.push_str(before);
        let named = before
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        out.push_str(if named { "Map<" } else { "BTreeMap<String, " });
        rest = after.get("Map<".len()..).unwrap_or_default();
    }
    out.push_str(rest);
    out
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
/// version bump, and history messages the catalogue lacks. A missing
/// committed file is `missing`'s level; a missing history message is an
/// error whenever the catalogue was read, because the app's history then
/// reads in the framework's generic words instead of its own.
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
            let committed: Vec<Value> = committed.iter().map(filed).collect();
            let reported: Vec<Value> = report.resources.iter().map(filed).collect();
            let old = keyed(&committed, "kind");
            let new = keyed(&reported, "kind");
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
                ..finding(Level::Error, "history-message", message)
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

/// Write the surface of the app containing `start` to its [`FILE`]: from
/// the report its binary printed for [`FLAG`], saved at `saved`, else from
/// running it. Returns the file and the report, whose unbound controls the
/// caller shows.
///
/// # Errors
/// Returns why the app does not report or the file cannot be written.
pub fn write_app(start: &Path, saved: Option<&Path>) -> Result<(PathBuf, SurfaceReport)> {
    let app_dir = crate::resolve_app_dir(start)?;
    let report = if let Some(saved) = saved {
        parse_report(
            &std::fs::read_to_string(saved)
                .with_context(|| format!("reading {}", saved.display()))?,
        )?
    } else {
        ensure_framework_app(&app_dir)?;
        report_of(&app_dir, None)?
    };
    let path = write(&app_dir, &report.resources)?;
    sync_resource_kinds(&app_dir)?;
    Ok((path, report))
}

/// Add to the manifest's `[resources] kinds`, which the package signs, the
/// app's own kinds it does not list yet: a resource added or renamed in
/// code would otherwise be one a governed room never lets the app create.
/// Kinds the manifest names already stay, whether the sources declare them
/// or not.
///
/// # Errors
/// Returns why the sources or the manifest do not read, or the manifest
/// cannot be written.
pub fn sync_resource_kinds(app_dir: &Path) -> Result<()> {
    let owned = crate::resource_kinds::owned_by_app(app_dir)?;
    let manifest = app_dir.join("tilo-app.toml");
    let text = std::fs::read_to_string(&manifest)
        .with_context(|| format!("reading {}", manifest.display()))?;
    let mut kinds: Vec<String> = toml::from_str::<toml::Table>(&text)
        .ok()
        .and_then(|table| {
            table
                .get("resources")?
                .get("kinds")?
                .clone()
                .try_into()
                .ok()
        })
        .unwrap_or_default();
    for kind in owned {
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    if let Some(updated) = with_resource_kinds(&text, &kinds) {
        std::fs::write(&manifest, updated)
            .with_context(|| format!("writing {}", manifest.display()))?;
    }
    Ok(())
}

/// `manifest` with its `[resources] kinds` set to `kinds`, or `None` when
/// it names them already (or the app declares none and the manifest has no
/// such table).
fn with_resource_kinds(manifest: &str, kinds: &[String]) -> Option<String> {
    let declared: Option<Vec<String>> =
        toml::from_str::<toml::Table>(manifest)
            .ok()
            .and_then(|table| {
                table
                    .get("resources")?
                    .get("kinds")?
                    .clone()
                    .try_into()
                    .ok()
            });
    if declared.as_deref() == Some(kinds) || (declared.is_none() && kinds.is_empty()) {
        return None;
    }
    let line = format!(
        "kinds = [{}]",
        kinds
            .iter()
            .map(|kind| format!("{kind:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut lines: Vec<String> = manifest.lines().map(str::to_owned).collect();
    if let Some(at) = lines.iter().position(|l| l.trim() == "[resources]") {
        let end = lines
            .iter()
            .skip(at.saturating_add(1))
            .position(|l| l.trim_start().starts_with('['))
            .map_or(lines.len(), |offset| {
                at.saturating_add(1).saturating_add(offset)
            });
        let existing = (at.saturating_add(1)..end).find(|&i| {
            lines
                .get(i)
                .is_some_and(|l| l.trim_start().starts_with("kinds"))
        });
        match existing {
            Some(i) => {
                if let Some(slot) = lines.get_mut(i) {
                    *slot = line;
                }
            }
            None => lines.insert(at.saturating_add(1), line),
        }
    } else {
        let before = lines
            .iter()
            .position(|l| l.trim() == "[publisher]")
            .unwrap_or(lines.len());
        lines.splice(
            before..before,
            ["[resources]".to_owned(), line, String::new()],
        );
    }
    let mut out = lines.join("\n");
    out.push('\n');
    Some(out)
}

fn ensure_framework_app(app_dir: &Path) -> Result<()> {
    if !is_framework_app(app_dir) {
        bail!(
            "{} does not run through `ui::main!(App)`, so its binary reports no surface",
            app_dir.display()
        );
    }
    Ok(())
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

/// Check the surface of the app at `start` against its committed [`FILE`]
/// (run from `executable` when it is built), with findings at `level`;
/// `None` for an app whose binary reports none. History messages are
/// asked of the app's source catalog when `level` is an error.
///
/// # Errors
/// Returns why the app does not report or the files do not read.
pub fn check_app(
    start: &Path,
    executable: Option<&Path>,
    level: Level,
) -> Result<Option<ContractReport>> {
    let app_dir = crate::resolve_app_dir(start)?;
    if !is_framework_app(&app_dir) {
        return Ok(None);
    }
    let report = report_of(&app_dir, executable)?;
    let catalog = match level {
        Level::Error => crate::contract::source_catalog(&app_dir)?,
        Level::Warning => None,
    };
    Ok(Some(findings(
        &app_id(&app_dir)?,
        &report,
        committed(&app_dir)?.as_deref(),
        catalog.as_ref(),
        level,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_names_the_kinds_the_sources_declare() {
        let kinds = vec!["so.example.table".to_owned()];
        let fresh = "[package]\nid = \"so.example\"\n\n[publisher]\nidentity = \"x\"\n";
        let added = with_resource_kinds(fresh, &kinds).unwrap();
        assert!(added.contains("[resources]\nkinds = [\"so.example.table\"]\n\n[publisher]"));
        assert_eq!(with_resource_kinds(&added, &kinds), None, "already named");
        let renamed = added.replace("so.example.table", "so.example.greeting");
        assert_eq!(
            with_resource_kinds(&renamed, &kinds).as_deref(),
            Some(added.as_str())
        );
        assert_eq!(with_resource_kinds(fresh, &[]), None, "nothing to declare");
    }

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
        let filed_now: Vec<Value> = now.resources.iter().map(filed).collect();
        assert_eq!(committed, filed_now);
        // What the file leaves out is not drift.
        assert!(committed[0]["fields"][0].get("title").is_none());
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

    /// The file a receiver reads holds what it decides with: verb kinds and
    /// footprints, field layouts and read-only marks, without the text that
    /// stays in the code.
    #[test]
    fn a_written_surface_reads_as_what_receivers_decide_with() {
        let mut book = resource(1, "String", &["rename"]);
        book["fields"][1]["ops"] = serde_json::json!({"type": "object", "title": "Text ops"});
        book["verbs"][0]["params"][0]["schema"]["description"] = "The new title".into();
        let text = render(&[book.clone()]).unwrap();
        assert!(!text.contains("title = \"Title\""), "{text}");
        assert!(!text.contains("label = \"rename\""), "{text}");
        assert!(!text.contains("The new title"), "{text}");
        let value: Value =
            serde_json::to_value(toml::from_str::<toml::Value>(&text).unwrap()).unwrap();
        let surface: tilo_actions::surface::Surface =
            serde_json::from_value(value["resources"][0].clone()).unwrap();
        assert_eq!(surface.verbs[0].id, "rename");
        assert!(surface.verbs[0].label.is_empty());
        assert_eq!(surface.fields[0].stored, "value");
        assert_eq!(surface.fields[1].stored, "collection");
        assert!(surface.fields[2].readonly);
        let ops = surface.fields[1].ops.clone().unwrap();
        assert!(ops.as_str().unwrap().starts_with("sha256:"));
        book["fields"][1]["ops"]["title"] = "Other ops".into();
        assert_ne!(filed(&book)["fields"][1]["ops"], ops);
        assert_eq!(render(&[value["resources"][0].clone()]).unwrap(), text);
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
                ("history-message", Level::Error)
            ]
        );
        assert!(
            !found.passed(),
            "an app whose history messages the catalogue lacks is not checked"
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

        // A plain map moved to a shared one is stored alike: drift only.
        let map = vec![normalized(&resource(
            1,
            "BTreeMap<String, Progress>",
            &["rename"],
        ))];
        let shared = report(&[resource(1, "tilo_resource::Map<Progress>", &["rename"])]);
        let found = findings("so.tilo.notes", &shared, Some(&map), None, Level::Error);
        assert_eq!(rules(&found), [("surface", Level::Error)]);
        let other = report(&[resource(1, "Map<Atom>", &["rename"])]);
        let found = findings("so.tilo.notes", &other, Some(&map), None, Level::Error);
        assert_eq!(
            rules(&found),
            [("storage", Level::Error), ("surface", Level::Error)]
        );

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
