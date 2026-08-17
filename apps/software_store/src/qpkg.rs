//! The qOS-native `.qpkg` data-source layer — the real qOS packaging path.
//!
//! Where [`crate::flatpak`] bridges to the `flatpak` CLI + the Flathub API,
//! this module bridges to qOS's own package manager, **`qos-install`**, and
//! the qOS app registry. Everything here runs on the background
//! [`crate::worker`] thread (subprocess / HTTP is allowed to block there).
//!
//! The store treats qOS apps as first-class alongside Flatpak: installed
//! qOS apps come from `qos-install list`, browsable apps come from the qOS
//! app registry, and install / remove shell out to the real `qos-install`
//! path — **never** flatpak. A qOS app is presented with its own source
//! label so the provenance is always honest.
//!
//! ## Commands (see `qos-install --help`)
//!
//! * `qos-install list`               — installed qOS apps (id, version, …)
//! * `qos-install update <id>`        — install / update by id from a registry
//! * `qos-install install <path>`     — install a local `.qpkg`
//! * `qos-install uninstall <id> --keep-state` — remove (non-interactive-safe)
//!
//! The browsable registry is an HTTP surface (`GET <base>/entries`) whose
//! base URL comes from `QOS_APP_REGISTRY`; there is no fabricated fallback
//! catalog, so with no registry configured the qOS browse set is simply
//! empty (honest), exactly like the Flathub path with no network.

use std::process::Command;
use std::time::Duration;

use crate::flatpak::{ActionOutcome, http_get, run_with_timeout};
use crate::model::{AppEntry, Category, Source};

/// The qOS package-manager binary. Package management lives here, **not** in
/// the `qos` OS-image CLI.
pub const QOS_INSTALL_BIN: &str = "qos-install";

/// One installed qOS app, parsed from `qos-install list`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QpkgApp {
    /// Reverse-DNS id, lowercase by convention (e.g. `dev.qos.notes`).
    pub id: String,
    /// Human display name (derived from the id when the listing has none).
    pub name: String,
    pub version: String,
}

/// Whether a usable `qos-install` binary is present. Uses the `path`
/// subcommand (prints the data root, exit 0) as a cheap liveness probe.
pub fn qos_available() -> bool {
    run_with_timeout(QOS_INSTALL_BIN, &["path"], Duration::from_millis(1500)).is_some()
}

// ===================================================================
// Installed set
// ===================================================================

/// `qos-install list` → the installed qOS apps.
pub fn list_installed() -> Vec<QpkgApp> {
    let Some(out) = run_with_timeout(QOS_INSTALL_BIN, &["list"], Duration::from_millis(2500))
    else {
        return Vec::new();
    };
    parse_list(&out)
}

/// Parse the fixed-width table `qos-install list` prints:
///
/// ```text
/// id                                version   pinned  attested-by  runtime  installed
/// dev.qos.notes                     0.3.1     no      -            -        2h ago
/// ```
///
/// Pure, so the real listing path is unit-testable without `qos-install`.
/// The header row and the `(no packages installed)` sentinel are skipped;
/// only rows whose first token is a reverse-DNS id (contains a `.`) are kept,
/// so stray banner lines never masquerade as apps.
pub fn parse_list(out: &str) -> Vec<QpkgApp> {
    let mut apps = Vec::new();
    for line in out.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.trim_start().starts_with("(no packages") {
            continue;
        }
        let mut cols = line.split_whitespace();
        let Some(id) = cols.next() else { continue };
        // Skip the header row and any non-id line.
        if id == "id" || !id.contains('.') {
            continue;
        }
        let version = cols.next().unwrap_or("").to_string();
        apps.push(QpkgApp { id: id.to_string(), name: display_name(id), version });
    }
    apps
}

/// Derive a readable display name from a reverse-DNS id: the last segment,
/// hyphens spaced, first letter upper-cased. `dev.qos.notes` → `Notes`.
pub fn display_name(id: &str) -> String {
    let last = id.rsplit('.').next().unwrap_or(id).replace('-', " ");
    let mut chars = last.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => id.to_string(),
    }
}

// ===================================================================
// Mutating actions (argv builders are pure + unit-tested)
// ===================================================================

/// Install / update a qOS app **by id** from a configured registry:
/// `qos-install update <id>`. This is the id-driven registry path
/// (`install` itself takes a local path, `update` resolves + fetches).
pub fn install_id_args(id: &str) -> Vec<String> {
    vec!["update".into(), id.into()]
}

/// Install a local `.qpkg`: `qos-install install <path>`.
pub fn install_path_args(path: &str) -> Vec<String> {
    vec!["install".into(), path.into()]
}

/// Remove an installed qOS app, non-interactive-safe:
/// `qos-install uninstall <id> --keep-state`. `--keep-state` is explicit so
/// the command never blocks waiting on a TTY prompt about saved data.
pub fn remove_args(id: &str) -> Vec<String> {
    vec!["uninstall".into(), id.into(), "--keep-state".into()]
}

/// Run a `qos-install` action to completion, capturing stderr on failure so
/// the UI shows a real reason. Honest: a non-zero exit is a real failure,
/// never a faked success.
pub fn run_action(args: &[String]) -> ActionOutcome {
    match Command::new(QOS_INSTALL_BIN).args(args).output() {
        Ok(o) if o.status.success() => ActionOutcome::Ok,
        Ok(o) => ActionOutcome::Failed(failure_reason(&String::from_utf8_lossy(&o.stderr))),
        Err(e) => ActionOutcome::Failed(format!("could not run qos-install: {e}")),
    }
}

/// Concise failure reason from `qos-install` stderr — its last non-empty
/// line, with the `qos-install: ` prefix stripped.
pub fn failure_reason(stderr: &str) -> String {
    let line = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .next_back()
        .unwrap_or("qos-install failed");
    line.strip_prefix("qos-install:").map(str::trim).unwrap_or(line).to_string()
}

// ===================================================================
// Registry browse (HTTP; no fabricated fallback)
// ===================================================================

/// The qOS app-registry base URL, from `QOS_APP_REGISTRY`. `None` → no
/// registry configured, so the qOS browse set is honestly empty (no fake
/// catalog), exactly like Flathub with no network.
pub fn registry_base() -> Option<String> {
    std::env::var("QOS_APP_REGISTRY").ok().filter(|s| !s.trim().is_empty())
}

/// Browse the qOS app registry: `GET <base>/entries` → store cards. Returns
/// `None` on any failure / no registry configured (never a fabricated set).
pub fn browse_registry() -> Option<Vec<AppEntry>> {
    let base = registry_base()?;
    let url = format!("{}/entries", base.trim_end_matches('/'));
    let resp = http_get(&url)?;
    parse_registry(&resp)
}

/// Parse the `/entries` JSON array (a `RegistryEntry` list) into qOS-native
/// [`AppEntry`]s. Pure, so the real browse path is unit-testable without the
/// network.
pub fn parse_registry(body: &str) -> Option<Vec<AppEntry>> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let arr = json.as_array()?;
    Some(arr.iter().filter_map(entry_from_registry).collect())
}

fn entry_from_registry(v: &serde_json::Value) -> Option<AppEntry> {
    let id = v.get("package_id").and_then(|x| x.as_str())?.to_string();
    let name = v
        .get("name")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| display_name(&id));
    let summary = v.get("description").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let category = v
        .get("categories")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|x| x.as_str())
        .map(category_from_str)
        .unwrap_or(Category::Productivity);

    let mut entry = AppEntry::flathub_stub(&id, &name, "qOS Project", &summary, category);
    entry.source = Source::QosNative;
    entry.remote = "qos".into();
    entry.monogram = name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
    if let Some(icon) = v.get("icon_url").and_then(|x| x.as_str()) {
        if icon.starts_with("http") {
            entry.icon_url = Some(icon.to_string());
        }
    }
    // Latest version + release date from the newest version entry.
    if let Some(ver) = v
        .get("versions")
        .and_then(|x| x.as_array())
        .and_then(|a| a.first())
        .and_then(|x| x.get("version"))
        .and_then(|x| x.as_str())
    {
        entry.latest_version = ver.to_string();
    }
    Some(entry)
}

/// Map a freedesktop-style category word to our coarse browse category.
fn category_from_str(c: &str) -> Category {
    match c.to_lowercase().as_str() {
        "graphics" | "photography" | "art" => Category::Graphics,
        "development" | "developertools" | "ide" => Category::Development,
        "game" | "games" => Category::Games,
        "network" | "internet" | "webbrowser" | "email" | "chat" => Category::Internet,
        _ => Category::Productivity,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_list_reads_installed_apps_and_skips_header_and_sentinel() {
        let out = "\
id                                version   pinned  attested-by         runtime             installed
dev.qos.notes                     0.3.1     no      -                   -                   2h ago
dev.qos.editor                    1.0.0     yes     acme-attestor       qos-runtime/1.2.0   5d ago
";
        let apps = parse_list(out);
        assert_eq!(apps.len(), 2, "header row is skipped, both apps parsed");
        assert_eq!(apps[0].id, "dev.qos.notes");
        assert_eq!(apps[0].version, "0.3.1");
        assert_eq!(apps[0].name, "Notes");
        assert_eq!(apps[1].id, "dev.qos.editor");
        assert_eq!(apps[1].version, "1.0.0");
    }

    #[test]
    fn parse_list_handles_empty_sentinel() {
        assert!(parse_list("(no packages installed)\n").is_empty());
        assert!(parse_list("").is_empty());
    }

    #[test]
    fn display_name_titlecases_last_segment() {
        assert_eq!(display_name("dev.qos.notes"), "Notes");
        assert_eq!(display_name("dev.qos.notes-cli"), "Notes cli");
        assert_eq!(display_name("solo"), "Solo");
    }

    #[test]
    fn action_args_route_through_qos_install_not_flatpak() {
        // The whole point: qOS apps install/remove via the real qOS path.
        assert_eq!(install_id_args("dev.qos.notes"), vec!["update", "dev.qos.notes"]);
        assert_eq!(install_path_args("/tmp/x.qpkg"), vec!["install", "/tmp/x.qpkg"]);
        assert_eq!(
            remove_args("dev.qos.notes"),
            vec!["uninstall", "dev.qos.notes", "--keep-state"],
            "remove is non-interactive-safe (--keep-state) so it never blocks on a prompt",
        );
    }

    #[test]
    fn failure_reason_strips_prefix_and_takes_last_line() {
        let err = "resolving dev.qos.x…\nqos-install: package dev.qos.x already installed (use --force)\n";
        assert_eq!(failure_reason(err), "package dev.qos.x already installed (use --force)");
        assert_eq!(failure_reason(""), "qos-install failed");
    }

    #[test]
    fn parse_registry_maps_entries_to_qos_native_apps() {
        let body = r#"[
            {
                "package_id": "dev.qos.editor",
                "name": "qOS Editor",
                "description": "A native code editor",
                "categories": ["Development"],
                "icon_url": "https://apps.qos.dev/icons/editor.png",
                "versions": [{"version": "1.0.0"}]
            }
        ]"#;
        let apps = parse_registry(body).expect("valid registry json");
        assert_eq!(apps.len(), 1);
        let a = &apps[0];
        assert_eq!(a.id, "dev.qos.editor");
        assert_eq!(a.name, "qOS Editor");
        assert_eq!(a.source, Source::QosNative, "registry entries are qOS-native, not flatpak");
        assert_eq!(a.category, Category::Development);
        assert_eq!(a.latest_version, "1.0.0");
        assert_eq!(a.icon_url.as_deref(), Some("https://apps.qos.dev/icons/editor.png"));
        assert_eq!(a.remote, "qos");
    }

    #[test]
    fn registry_base_absent_yields_no_browse() {
        // No fabricated fallback: with no registry configured, browse is empty.
        // (We can't safely mutate process env in parallel tests, so assert the
        // parse-side contract: an empty array parses to an empty set.)
        assert!(parse_registry("[]").unwrap().is_empty());
    }
}
