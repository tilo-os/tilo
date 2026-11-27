//! What every app commits about itself for people to find it by, gathered at build time.
//!
//! Each app commits its catalogue entry beside itself — `fleet.json` next to a native app's
//! manifest, next to a tilolang package's `app.tl` — written and checked by `tilolang describe
//! --fleet --write` and `--check`. This script reads those files, one per app, and nothing else:
//! no app is compiled, and no file every app shares is written from all of them. A change to one
//! app's entry rebuilds this crate; a change to anything else does not.

use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// An action an app offers while it is closed (`[[offers]]`), as its entry names it.
#[derive(Deserialize)]
struct Offer {
    resource: String,
    resource_kind: String,
    action: String,
    label: String,
    description: String,
    kind: String,
    operation: String,
    confirm: bool,
    asks: Vec<Asked>,
    keywords: Vec<String>,
    icon: String,
}

/// One value an offered action asks for.
#[derive(Deserialize)]
struct Asked {
    name: String,
    #[serde(rename = "type")]
    ty: String,
    description: String,
}

/// One action an app's windows run on keys.
#[derive(Deserialize)]
struct Command {
    action: String,
    label: String,
    key: String,
}

/// An app's catalogue entry, as the app commits it.
#[derive(Deserialize)]
struct Entry {
    id: String,
    name: String,
    category: String,
    platforms: Vec<String>,
    tagline: String,
    names: Vec<String>,
    keywords: Vec<String>,
    /// `(kind, label)` of each kind of record it creates.
    resource_types: Vec<(String, String)>,
    offers: Vec<Offer>,
    commands: Vec<Command>,
    /// Whether search and the Store offer it.
    listed: bool,
    /// Its compiled surface, when the app generates it from its own source.
    #[serde(default)]
    surface: Option<String>,
    /// The file beside the app that holds its compiled surface.
    #[serde(default)]
    surface_file: Option<String>,
}

/// One app and its compiled surface: empty when it ships none.
struct App {
    entry: Entry,
    surface: String,
}

/// A service whose actions a workspace answers, as the fleet names it: its id, the
/// name people call it, and the surface file its own code writes beside its crate. Its actions are
/// the workspace's, so a program of another language reaches them as it reaches an app's.
const SERVICES: [(&str, &str, &str); 2] = [
    (
        "tilo.workspace",
        "Workspace",
        "crates/core/tilo-workspace-service/workspace-surface.json",
    ),
    (
        "tilo.account",
        "Account",
        "crates/core/tilo-api/account-surface.json",
    ),
];

fn main() -> Result<(), Box<dyn Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .map_err(|error| format!("a Tilo checkout: {error}"))?;
    let apps_dir = root.join("apps");
    if !apps_dir.is_dir() {
        // A packaged crate carries no apps: it is built from its own sources alone, where no app
        // is known, so it compiles an empty fleet and says so.
        println!(
            "cargo::warning=tilo-fleet-catalogue: {} holds no apps, so this build compiles an empty fleet",
            apps_dir.display()
        );
        return write(&render(&[], &[]));
    }
    let mut apps: Vec<App> = Vec::new();
    watch(&apps_dir);
    // The native apps first, in the order their ids sort: a manifest and the entry beside it.
    for dir in dirs(&apps_dir) {
        watch(&dir);
        if !dir.join("tilo-app.toml").is_file() {
            continue;
        }
        apps.push(read(&dir)?);
    }
    // Then the tilolang packages that declare an app; one that declares none commits no entry.
    let tilolang = apps_dir.join("tl");
    watch(&tilolang);
    for dir in dirs(&tilolang) {
        watch(&dir);
        if dir.join("fleet.json").is_file() {
            apps.push(read(&dir)?);
        }
    }
    // Each settings section is a resource of the workspace whichever app shows it, typed by the
    // surface the sections publish: a package in a crate of its own.
    let sections = root.join("crates/desktop/settings-sections/published.json");
    watch(&sections);
    let sections_surface = fs::read_to_string(&sections)?;
    // The services whose actions are the workspace's, each from the file its own code writes.
    let mut services: Vec<(&str, &str, String)> = SERVICES
        .iter()
        .map(|(id, name, file)| {
            let path = root.join(file);
            watch(&path);
            Ok((*id, *name, fs::read_to_string(&path)?))
        })
        .collect::<Result<_, std::io::Error>>()?;
    services.push((
        "so.tilo.settings.sections",
        "Settings sections",
        sections_surface,
    ));
    claim_each_id(&apps)?;
    write(&render(&apps, &services))
}

/// Write the fleet's module into `OUT_DIR`, where it is included from.
fn write(module: &str) -> Result<(), Box<dyn Error>> {
    let out_dir = env::var("OUT_DIR")?;
    fs::write(Path::new(&out_dir).join("fleet.rs"), module)?;
    Ok(())
}

/// `dir`'s committed catalogue entry and the surface it points at.
fn read(dir: &Path) -> Result<App, Box<dyn Error>> {
    let file = dir.join("fleet.json");
    if !file.is_file() {
        return Err(format!(
            "{}: no fleet.json: write it with `tilolang describe --fleet --write`",
            file.display()
        )
        .into());
    }
    watch(&file);
    let text = fs::read_to_string(&file)?;
    let entry: Entry =
        serde_json::from_str(&text).map_err(|error| format!("{}: {error}", file.display()))?;
    let surface = match (&entry.surface, &entry.surface_file) {
        (Some(surface), _) => surface.clone(),
        (None, Some(name)) => {
            let path = dir.join(name);
            watch(&path);
            fs::read_to_string(&path)?
        }
        (None, None) => String::new(),
    };
    Ok(App { entry, surface })
}

/// Rerun this script when `path` changes.
fn watch(path: &Path) {
    println!("cargo::rerun-if-changed={}", path.display());
}

/// Add one line to the module.
fn line(out: &mut String, text: &str) {
    out.push_str(text);
    out.push('\n');
}

/// The directories under `dir`, in the order files sort; none when `dir` does not exist.
fn dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// Refuse two apps claimed by one id: a device could not tell them apart.
fn claim_each_id(apps: &[App]) -> Result<(), Box<dyn Error>> {
    let mut claimed: Vec<&str> = Vec::new();
    for app in apps {
        let id = app.entry.id.as_str();
        if claimed.contains(&id) {
            return Err(format!("{id} is claimed by two apps").into());
        }
        claimed.push(id);
    }
    Ok(())
}

/// The fleet's Rust module: every listed app's entry, and every package's surfaced declarations.
fn render(apps: &[App], services: &[(&str, &str, String)]) -> String {
    let mut out = String::from(
        "// What every app commits about itself, gathered by this crate's build script from each\n\
         // app's `fleet.json` (`tilolang describe --fleet --write`). Not edited by hand.\n\n\
         use super::FleetApp;\n\n",
    );
    let listed: Vec<&App> = apps.iter().filter(|app| app.entry.listed).collect();
    out.push_str(&surfaces(apps, services));
    out.push_str(&surface_names(apps, services));
    out.push_str("pub(super) const APPS: &[FleetApp] = &[\n");
    for app in &listed {
        out.push_str(&app_module(app));
    }
    out.push_str("];\n");
    out
}

/// Each package's compiled surface, as its package ships it.
fn surfaces(apps: &[App], services: &[(&str, &str, String)]) -> String {
    let mut out = String::from(
        "/// Each package's compiled surface, as its package ships it: what a task's programs\n\
         /// compile against there, and what the SDK's types and the API's description describe,\n\
         /// by the package's id. A service's surface is here too: its actions are the workspace's.\n\
         /// The `surfaces` feature carries it; apps and the web desktop leave it off.\n\
         #[cfg(feature = \"surfaces\")]\n\
         pub(super) const SURFACES: &[(&str, &str)] = &[\n",
    );
    for app in apps.iter().filter(|app| !app.surface.is_empty()) {
        line(
            &mut out,
            &format!("    ({}, {}),", quoted(&app.entry.id), quoted(&app.surface)),
        );
    }
    for (id, _, surface) in services {
        line(
            &mut out,
            &format!("    ({}, {}),", quoted(id), quoted(surface)),
        );
    }
    out.push_str("];\n\n");
    out
}

/// The name of each package that ships a surface.
fn surface_names(apps: &[App], services: &[(&str, &str, String)]) -> String {
    let mut out = String::from(
        "/// The name of each package that ships a surface (`SURFACES`), including the services\n\
         /// the fleet does not list.\n\
         #[cfg(feature = \"surfaces\")]\n\
         pub(super) const SURFACE_NAMES: &[(&str, &str)] = &[\n",
    );
    for app in apps.iter().filter(|app| !app.surface.is_empty()) {
        line(
            &mut out,
            &format!(
                "    ({}, {}),",
                quoted(&app.entry.id),
                quoted(&app.entry.name)
            ),
        );
    }
    for (id, name, _) in services {
        line(
            &mut out,
            &format!("    ({}, {}),", quoted(id), quoted(name)),
        );
    }
    out.push_str("];\n\n");
    out
}

/// One entry of the fleet's `APPS`.
fn app_module(app: &App) -> String {
    let entry = &app.entry;
    let mut out = String::from("    FleetApp {\n");
    line(&mut out, &format!("        id: {},", quoted(&entry.id)));
    line(&mut out, &format!("        name: {},", quoted(&entry.name)));
    line(
        &mut out,
        &format!("        category: {},", quoted(&entry.category)),
    );
    line(
        &mut out,
        &format!("        platforms: {},", literals(&entry.platforms)),
    );
    line(
        &mut out,
        &format!("        tagline: {},", quoted(&entry.tagline)),
    );
    line(
        &mut out,
        &format!("        names: {},", literals(&entry.names)),
    );
    line(
        &mut out,
        &format!("        keywords: {},", literals(&entry.keywords)),
    );
    let types: Vec<String> = entry
        .resource_types
        .iter()
        .map(|(kind, label)| format!("({}, {})", quoted(kind), quoted(label)))
        .collect();
    line(
        &mut out,
        &format!("        resource_types: &[{}],", types.join(", ")),
    );
    let mut offers = String::new();
    for offer in &entry.offers {
        line(&mut offers, &offer_line(offer));
    }
    line(&mut out, &format!("        offers: {},", quoted(&offers)));
    let mut commands = String::new();
    for command in &entry.commands {
        line(
            &mut commands,
            &[
                command.action.as_str(),
                command.label.as_str(),
                command.key.as_str(),
            ]
            .join("\t"),
        );
    }
    line(
        &mut out,
        &format!("        commands: {},", quoted(&commands)),
    );
    out.push_str("    },\n");
    out
}

/// One line of an app's offers: its fields in order, each after a tab.
fn offer_line(offer: &Offer) -> String {
    [
        offer.resource.clone(),
        offer.resource_kind.clone(),
        offer.action.clone(),
        offer.label.clone(),
        offer.description.clone(),
        offer.kind.clone(),
        offer.operation.clone(),
        String::from(if offer.confirm { "1" } else { "0" }),
        asks(&offer.asks),
        offer.keywords.join("\u{1f}"),
        offer.icon.clone(),
    ]
    .join("\t")
}

/// What an offer's values ask for: name, type and words per value, values after a record
/// separator and a value's fields after a group separator.
fn asks(asked: &[Asked]) -> String {
    let parts: Vec<String> = asked
        .iter()
        .map(|asked| {
            [
                asked.name.as_str(),
                asked.ty.as_str(),
                asked.description.as_str(),
            ]
            .join("\u{1d}")
        })
        .collect();
    parts.join("\u{1e}")
}

/// `value` as a Rust string literal.
fn quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len().saturating_add(2));
    out.push('"');
    for character in value.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{1e}' => out.push_str("\\u{1e}"),
            '\u{1d}' => out.push_str("\\u{1d}"),
            '\u{1f}' => out.push_str("\\u{1f}"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// `items` as a slice of Rust string literals.
fn literals(items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|item| quoted(item)).collect();
    format!("&[{}]", quoted.join(", "))
}
