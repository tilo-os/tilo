//! The Flatpak data-source layer — the bridge to the real world.
//!
//! Everything that touches the `flatpak` CLI or the Flathub HTTP API lives
//! here, behind small pure-ish functions that return owned data. NONE of
//! these functions are called on the UI thread: they all run inside the
//! background [`crate::worker`] thread, which is why every one of them is
//! allowed to block on a subprocess or a socket. The widget only ever
//! sees the *parsed result* arriving over a channel.
//!
//! ## Data paths
//!
//! Local system state (what's installed, which remotes exist) comes from
//! the `flatpak` CLI in machine-readable `--columns` mode:
//!
//! * `flatpak list --app --columns=application,version,size,origin`
//! * `flatpak remotes --columns=name,url,title`
//! * `flatpak remote-ls <remote> --app --columns=application,name,...`
//!
//! Rich catalog metadata (summaries, screenshots, descriptions, download
//! counts) comes from the **Flathub API v2** (`https://flathub.org/api/v2`):
//!
//! * `POST /search`                         — full-text search
//! * `GET  /collection/popular/<page>`      — the popular/featured set
//! * `GET  /collection/recently-updated/<page>`
//! * `GET  /appstream/<app-id>`             — full AppStream record for the
//!                                            detail page (screenshots, etc.)
//! * `GET  /stats/<app-id>`                 — download totals (our proxy for
//!                                            "popularity", since Flathub
//!                                            exposes no ratings/reviews API)
//!
//! Ratings & textual reviews are **not** offered by any Flathub endpoint,
//! so for live apps we synthesise a star rating from the download stats
//! and surface "N downloads" as the social-proof signal; the bundled
//! sample catalog carries hand-written reviews so the offline `--shots`
//! detail page is fully populated. This is called out in the UI with a
//! "ratings are estimated from download counts" note.
//!
//! Mutating actions (`install`, `update`, `uninstall`, `remote-add`,
//! `remote-delete`) shell out to `flatpak` with `--noninteractive` and are
//! always gated behind an explicit user confirmation in the UI before the
//! worker is asked to run them.

use std::collections::BTreeMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use crate::model::{AppEntry, Category, InstallPhase, Permission, Review, Shot};

/// Which Flatpak *installation* an operation targets. Flathub is commonly
/// registered in **both** the system and the per-user installation, so a
/// bare `flatpak install flathub <ref>` is ambiguous and fails in
/// non-interactive mode with `Remote 'flathub' found in multiple
/// installations`. Pinning the installation with `--user` / `--system`
/// resolves the remote to exactly one installation, which is what makes the
/// mutating commands work non-interactively.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The per-user installation (`~/.local/share/flatpak`). Our default — a
    /// non-root desktop session installs here, and it needs no privilege
    /// escalation.
    User,
    /// The system installation (`/var/lib/flatpak`).
    System,
}

impl Scope {
    /// The CLI flag that pins a command to this installation.
    pub fn flag(self) -> &'static str {
        match self {
            Scope::User => "--user",
            Scope::System => "--system",
        }
    }

    /// A short human label for the provenance line ("User" / "System").
    pub fn label(self) -> &'static str {
        match self {
            Scope::User => "User",
            Scope::System => "System",
        }
    }
}

/// Base URL for the Flathub API v2. Overridable via `FLATHUB_API_BASE`
/// for tests / mirrors.
fn flathub_api_base() -> String {
    std::env::var("FLATHUB_API_BASE").unwrap_or_else(|_| "https://flathub.org/api/v2".to_string())
}

/// Run `program args...`, returning stdout as a String, or `None` if the
/// binary is missing, exits non-zero, or doesn't answer within `timeout`.
///
/// This is the single choke-point that guarantees a missing `flatpak`
/// binary or a hung network mount degrades to "no data" instead of
/// freezing the worker thread forever.
pub fn run_with_timeout(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let program = program.to_string();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let out = Command::new(&program)
            .args(&args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
        let _ = tx.send(out);
    });
    match rx.recv_timeout(timeout) {
        Ok(v) => v,
        Err(_) => None,
    }
}

/// Run `program args...`, returning stdout as **raw bytes**, or `None` on
/// failure/timeout. The binary-safe sibling of [`run_with_timeout`] — used
/// to fetch encoded image bytes (PNG/JPEG/WebP) that a lossy UTF-8 decode
/// would corrupt.
pub fn run_bytes_with_timeout(program: &str, args: &[&str], timeout: Duration) -> Option<Vec<u8>> {
    let program = program.to_string();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let out = Command::new(&program)
            .args(&args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| o.stdout);
        let _ = tx.send(out);
    });
    match rx.recv_timeout(timeout) {
        Ok(v) => v,
        Err(_) => None,
    }
}

/// Fetch the raw bytes of a remote image over HTTP(S) with `curl`, bounded so
/// a slow CDN can't wedge the fetch thread. Used by the store's image loader
/// to pull real Flathub app icons + screenshots off the UI thread. `None` on
/// any failure — the caller shows an honest loading/fallback state.
pub fn http_get_bytes(url: &str) -> Option<Vec<u8>> {
    let bytes = run_bytes_with_timeout(
        "curl",
        &["-sSL", "--max-time", "12", url],
        Duration::from_secs(15),
    )?;
    if bytes.is_empty() { None } else { Some(bytes) }
}

/// Whether a usable `flatpak` binary is on PATH and answers `--version`.
pub fn flatpak_available() -> bool {
    run_with_timeout("flatpak", &["--version"], Duration::from_millis(1500)).is_some()
}

// ===================================================================
// Local CLI queries (installed set + remotes)
// ===================================================================

/// One configured Flatpak remote, parsed from `flatpak remotes`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
    pub url: String,
    pub title: String,
    /// `true` when the remote is disabled (`Options` contains `disabled`).
    pub disabled: bool,
    /// Which installation this remote is configured in, when known. A remote
    /// (Flathub especially) can exist in **both** the user and system
    /// installations; we tag each listing with its scope so installs can pin
    /// the installation explicitly and avoid the "found in multiple
    /// installations" ambiguity. `None` when the scope wasn't resolved.
    pub scope: Option<Scope>,
}

/// One installed Flatpak app, resolved across installations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledApp {
    pub id: String,
    pub name: String,
    pub version: String,
    /// The remote the app was installed from (`origin` column).
    pub origin: String,
    /// The installation it lives in — so update/uninstall can pin the scope.
    pub scope: Scope,
}

/// `flatpak list --app --columns=application,version` → app-id → version.
///
/// Queries **both** installations (user + system) and merges, so an app
/// installed in either shows up. Kept returning `id → version` for the
/// callers that only need the version map; [`list_installed_apps`] is the
/// richer sibling that also carries the name / origin / scope.
pub fn list_installed() -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for app in list_installed_apps() {
        map.entry(app.id).or_insert(app.version);
    }
    map
}

/// List installed apps across **all** installations (user + system), each
/// tagged with the installation it lives in. This is what fixes the
/// "Installed · 0" bug: a bare `flatpak list` on some hosts only reports one
/// installation, so we query each scope explicitly and union the results.
pub fn list_installed_apps() -> Vec<InstalledApp> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for scope in [Scope::User, Scope::System] {
        if let Some(text) = run_with_timeout(
            "flatpak",
            &["list", "--app", scope.flag(), "--columns=application,name,version,origin"],
            Duration::from_millis(2000),
        ) {
            for app in parse_installed_apps(&text, scope) {
                if seen.insert(app.id.clone()) {
                    out.push(app);
                }
            }
        }
    }
    out
}

/// Parse the tab-separated `application\tname\tversion\torigin` output of
/// `flatpak list --columns=application,name,version,origin` into
/// [`InstalledApp`]s, tagging each with `scope`. Pure, so the real
/// cross-installation listing path is unit-testable without `flatpak`.
pub fn parse_installed_apps(out: &str, scope: Scope) -> Vec<InstalledApp> {
    let mut apps = Vec::new();
    for line in out.lines() {
        let mut parts = line.split('\t');
        let Some(id) = parts.next() else { continue };
        let id = id.trim();
        if id.is_empty() {
            continue;
        }
        let name = parts.next().unwrap_or("").trim();
        let version = parts.next().unwrap_or("").trim().to_string();
        let origin = parts.next().unwrap_or("").trim().to_string();
        apps.push(InstalledApp {
            id: id.to_string(),
            name: if name.is_empty() { id.to_string() } else { name.to_string() },
            version,
            origin,
            scope,
        });
    }
    apps
}

/// Parse the tab-separated `application\tversion` output of
/// `flatpak list` into an app-id → version map. Pure, so the real listing
/// path is unit-testable without the `flatpak` binary.
pub fn parse_installed(out: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in out.lines() {
        let mut parts = line.split('\t');
        if let Some(id) = parts.next() {
            let id = id.trim();
            if id.is_empty() {
                continue;
            }
            let version = parts.next().unwrap_or("").trim().to_string();
            map.insert(id.to_string(), version);
        }
    }
    map
}

/// List configured remotes across **both** installations (user + system),
/// each tagged with its scope. Querying per-installation is what lets an
/// install pin the right `--user` / `--system` flag when a remote (Flathub)
/// exists in more than one.
pub fn list_remotes() -> Vec<Remote> {
    let mut out = Vec::new();
    for scope in [Scope::User, Scope::System] {
        if let Some(text) = run_with_timeout(
            "flatpak",
            &["remotes", scope.flag(), "--columns=name,url,title,options"],
            Duration::from_millis(1500),
        ) {
            out.extend(parse_remotes_scoped(&text, Some(scope)));
        }
    }
    out
}

/// Parse the tab-separated `name\turl\ttitle\toptions` output of
/// `flatpak remotes` into [`Remote`]s (scope unknown). Pure, so the real
/// remote-listing path is unit-testable without the `flatpak` binary.
pub fn parse_remotes(out: &str) -> Vec<Remote> {
    parse_remotes_scoped(out, None)
}

/// Scope-tagging variant of [`parse_remotes`].
pub fn parse_remotes_scoped(out: &str, scope: Option<Scope>) -> Vec<Remote> {
    let mut remotes = Vec::new();
    for line in out.lines() {
        let mut parts = line.split('\t');
        let Some(name) = parts.next() else { continue };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let url = parts.next().unwrap_or("").trim().to_string();
        let title = parts.next().unwrap_or("").trim().to_string();
        let options = parts.next().unwrap_or("");
        remotes.push(Remote {
            name: name.to_string(),
            title: if title.is_empty() { name.to_string() } else { title },
            url,
            disabled: options.contains("disabled"),
            scope,
        });
    }
    remotes
}

/// Which installation `app_id` is installed in, if any — so update/uninstall
/// can pin the right `--user`/`--system` scope. A live query across both
/// installations.
pub fn installed_scope(app_id: &str) -> Option<Scope> {
    list_installed_apps().into_iter().find(|a| a.id == app_id).map(|a| a.scope)
}

/// Pick the installation to install `remote` from, given the known set of
/// remotes. Prefers the **user** installation (no privilege escalation, the
/// desktop-session default) when the remote exists there, otherwise falls
/// back to whichever installation has it, and finally to `--user`. This is
/// the disambiguation that turns an ambiguous `flatpak install flathub …`
/// into an unambiguous `flatpak install --user flathub …`.
pub fn resolve_install_scope(remote: &str, remotes: &[Remote]) -> Scope {
    let scoped = |want: Scope| {
        remotes
            .iter()
            .any(|r| r.name == remote && !r.disabled && r.scope == Some(want))
    };
    if scoped(Scope::User) {
        Scope::User
    } else if scoped(Scope::System) {
        Scope::System
    } else {
        Scope::User
    }
}

// ===================================================================
// Mutating actions (always gated in the UI before being requested)
// ===================================================================

/// Outcome of a mutating flatpak action, reported back to the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActionOutcome {
    Ok,
    Failed(String),
}

/// A live progress sample streamed out of a running `flatpak` transaction.
/// `fraction` is `None` until flatpak emits a parseable percentage — the UI
/// shows an honest indeterminate "running" state until then, never a faked
/// bar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ActionProgress {
    pub fraction: Option<f64>,
    pub phase: Option<InstallPhase>,
}

// ---- argv builders (pure; the disambiguation lives here, unit-testable) ----

/// The argv for a gated install, pinned to `scope` and its remote so it is
/// **unambiguous and non-interactive-safe**: `flatpak install --user
/// --noninteractive <remote> <app-id>`. Pinning the installation is what
/// avoids `Remote '<remote>' found in multiple installations`. The remote is
/// named explicitly so flatpak resolves the ref against exactly one source.
pub fn install_args(scope: Scope, remote: &str, app_id: &str) -> Vec<String> {
    vec![
        "install".into(),
        scope.flag().into(),
        "--noninteractive".into(),
        remote.into(),
        app_id.into(),
    ]
}

/// The argv for a gated update, pinned to the installation the app lives in:
/// `flatpak update --user --noninteractive <app-id>`.
pub fn update_args(scope: Scope, app_id: &str) -> Vec<String> {
    vec!["update".into(), scope.flag().into(), "--noninteractive".into(), app_id.into()]
}

/// The argv for a gated uninstall, pinned to the installation the app lives
/// in: `flatpak uninstall --user --noninteractive <app-id>`.
pub fn uninstall_args(scope: Scope, app_id: &str) -> Vec<String> {
    vec!["uninstall".into(), scope.flag().into(), "--noninteractive".into(), app_id.into()]
}

/// The argv for a `.flatpakref` install, pinned to the user installation so
/// it runs non-interactively without a privilege prompt.
pub fn install_ref_args(scope: Scope, path: &str) -> Vec<String> {
    vec!["install".into(), scope.flag().into(), "--noninteractive".into(), path.into()]
}

/// `true` if `stderr` carries flatpak's "found in multiple installations"
/// ambiguity — the exact failure the scope-pinned argv above prevents. We
/// still detect it so a stray un-pinned path surfaces a clear message rather
/// than a raw dump.
pub fn is_multiple_installations_error(stderr: &str) -> bool {
    let s = stderr.to_lowercase();
    s.contains("multiple installations")
}

/// Parse a live progress fraction (0.0..=1.0) out of one line/chunk of
/// flatpak output. flatpak prints a percentage like `… 42%` while a
/// transaction runs; we pull the last `NN%` token so a `\r`-updated line
/// yields its most recent value. `None` when the chunk carries no percent.
pub fn parse_progress_fraction(line: &str) -> Option<f64> {
    let bytes = line.as_bytes();
    let mut best: Option<f64> = None;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            // Walk back over the digits immediately before the '%'.
            let mut j = i;
            while j > 0 && bytes[j - 1].is_ascii_digit() {
                j -= 1;
            }
            if j < i {
                if let Ok(n) = line[j..i].parse::<u32>() {
                    if n <= 100 {
                        best = Some(n as f64 / 100.0);
                    }
                }
            }
        }
        i += 1;
    }
    best
}

/// Detect the transaction phase from a flatpak output chunk, so the UI can
/// label "Downloading" vs "Installing" honestly from the real stream.
pub fn parse_progress_phase(line: &str) -> Option<InstallPhase> {
    let l = line.to_lowercase();
    if l.contains("installing") || l.contains("deploying") || l.contains("updating") {
        Some(InstallPhase::Installing)
    } else if l.contains("downloading") || l.contains("fetching") || l.contains("pulling") {
        Some(InstallPhase::Downloading)
    } else {
        None
    }
}

/// `flatpak remote-add --if-not-exists --user <name> <url>`.
pub fn remote_add(name: &str, url: &str) -> ActionOutcome {
    run_action(&["remote-add", "--if-not-exists", "--user", name, url])
}

/// `flatpak remote-delete --force <name>`.
pub fn remote_delete(name: &str) -> ActionOutcome {
    run_action(&["remote-delete", "--force", name])
}

/// `flatpak run <app-id>` — launch an installed app (fire-and-forget).
pub fn run_app(app_id: &str) {
    let _ = Command::new("flatpak").args(["run", app_id]).spawn();
}

/// Run a `flatpak` action to completion, **streaming** its output so the UI
/// reflects the real transaction state: `on_progress` is called with each
/// parsed [`ActionProgress`] sample as flatpak emits it, and the returned
/// [`ActionOutcome`] is the process's real exit result (success, or the last
/// stderr line as the failure reason). No wall-clock faking — an install
/// that fails ends as [`ActionOutcome::Failed`], never a full bar.
pub fn run_action_streaming(
    args: &[String],
    on_progress: impl FnMut(ActionProgress),
) -> ActionOutcome {
    run_program_streaming("flatpak", args, on_progress)
}

/// Program-parameterised core of [`run_action_streaming`] — lets tests drive
/// the real streaming/parse/outcome path with a stand-in command that emits
/// flatpak-shaped progress, rather than mocking away the code under test.
pub fn run_program_streaming(
    program: &str,
    args: &[String],
    mut on_progress: impl FnMut(ActionProgress),
) -> ActionOutcome {
    let mut child = match Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return ActionOutcome::Failed(format!("could not run {program}: {e}")),
    };

    // Drain stderr on its own thread so a full pipe can't deadlock the child;
    // keep the tail for the failure message.
    let stderr = child.stderr.take();
    let (etx, erx) = mpsc::channel::<String>();
    let ehandle = std::thread::spawn(move || {
        let mut captured = String::new();
        if let Some(mut e) = stderr {
            let mut buf = Vec::new();
            let _ = e.read_to_end(&mut buf);
            captured = String::from_utf8_lossy(&buf).into_owned();
        }
        let _ = etx.send(captured);
    });

    // Stream stdout, splitting on \r and \n so `\r`-updated progress lines are
    // surfaced as they change.
    if let Some(mut out) = child.stdout.take() {
        let mut buf = [0u8; 4096];
        let mut line = String::new();
        loop {
            match out.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    for &b in &buf[..n] {
                        if b == b'\n' || b == b'\r' {
                            emit_progress(&line, &mut on_progress);
                            line.clear();
                        } else {
                            line.push(b as char);
                        }
                    }
                }
                Err(_) => break,
            }
        }
        emit_progress(&line, &mut on_progress);
    }

    let status = child.wait();
    let stderr_text = erx.recv().unwrap_or_default();
    let _ = ehandle.join();

    match status {
        Ok(s) if s.success() => ActionOutcome::Ok,
        Ok(_) => ActionOutcome::Failed(failure_reason(&stderr_text)),
        Err(e) => ActionOutcome::Failed(format!("could not run {program}: {e}")),
    }
}

/// Emit a progress sample for `chunk` if it carries a percentage or a phase
/// keyword — so an indeterminate stretch produces no misleading fraction.
fn emit_progress(chunk: &str, on_progress: &mut impl FnMut(ActionProgress)) {
    let fraction = parse_progress_fraction(chunk);
    let phase = parse_progress_phase(chunk);
    if fraction.is_some() || phase.is_some() {
        on_progress(ActionProgress { fraction, phase });
    }
}

/// Turn a captured stderr blob into a concise, honest failure reason — the
/// last non-empty line, with the multiple-installations ambiguity flagged
/// clearly should it ever slip through.
pub fn failure_reason(stderr: &str) -> String {
    if is_multiple_installations_error(stderr) {
        return "remote is in multiple installations — retry with an explicit installation".into();
    }
    let msg = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .next_back()
        .unwrap_or("flatpak failed");
    if msg.is_empty() { "flatpak failed".into() } else { msg.to_string() }
}

/// Run a non-streaming `flatpak` action (remote add/delete), capturing
/// stderr on failure.
fn run_action(args: &[&str]) -> ActionOutcome {
    let out = Command::new("flatpak").args(args).output();
    match out {
        Ok(o) if o.status.success() => ActionOutcome::Ok,
        Ok(o) => ActionOutcome::Failed(failure_reason(&String::from_utf8_lossy(&o.stderr))),
        Err(e) => ActionOutcome::Failed(format!("could not run flatpak: {e}")),
    }
}

// ===================================================================
// Flathub HTTP API (catalog metadata, search, stats)
// ===================================================================

/// Fetch `url` over HTTP, returning the body. Uses `curl` so we don't pull
/// a TLS stack into the dependency closure (qOS ships `curl` in the base
/// image). `None` on any failure / timeout — the worker treats that as
/// "no live catalog, keep the cached/sample data".
pub fn http_get(url: &str) -> Option<String> {
    run_with_timeout(
        "curl",
        &["-sSL", "--max-time", "8", "-H", "Accept: application/json", url],
        Duration::from_secs(10),
    )
}

/// POST a JSON body to `url`, returning the response body.
pub fn http_post_json(url: &str, body: &str) -> Option<String> {
    run_with_timeout(
        "curl",
        &[
            "-sSL", "--max-time", "8", "-X", "POST", "-H", "Content-Type: application/json", "-H",
            "Accept: application/json", "-d", body, url,
        ],
        Duration::from_secs(10),
    )
}

/// Search Flathub for `query`, returning catalog entries (best-effort).
///
/// Hits `POST /search` with `{"query": ...}` and maps each hit into our
/// [`AppEntry`]. Screenshots / full descriptions are deliberately left
/// thin here (the search payload is summary-level); the detail page lazily
/// fetches the full AppStream record via [`appstream`].
pub fn search(query: &str) -> Option<Vec<AppEntry>> {
    let url = format!("{}/search", flathub_api_base());
    let body = serde_json::json!({ "query": query }).to_string();
    let resp = http_post_json(&url, &body)?;
    let json: serde_json::Value = serde_json::from_str(&resp).ok()?;
    // The search response is `{ "hits": [ { ... } ] }`.
    let hits = json.get("hits").and_then(|h| h.as_array())?;
    Some(hits.iter().filter_map(entry_from_search_hit).collect())
}

/// Fetch a Flathub *collection* (e.g. `popular`, `recently-updated`) as a
/// page of catalog entries.
pub fn collection(name: &str, page: u32, per_page: u32) -> Option<Vec<AppEntry>> {
    let url = format!(
        "{}/collection/{}?page={}&per_page={}",
        flathub_api_base(),
        name,
        page,
        per_page
    );
    let resp = http_get(&url)?;
    let json: serde_json::Value = serde_json::from_str(&resp).ok()?;
    let hits = json.get("hits").and_then(|h| h.as_array())?;
    Some(hits.iter().filter_map(entry_from_search_hit).collect())
}

/// Fetch the full AppStream record for `app_id` and merge it into a richer
/// [`AppEntry`] (description, screenshots, permissions, size, version).
pub fn appstream(app_id: &str) -> Option<AppEntry> {
    let url = format!("{}/appstream/{}", flathub_api_base(), app_id);
    let resp = http_get(&url)?;
    let json: serde_json::Value = serde_json::from_str(&resp).ok()?;
    entry_from_appstream(app_id, &json)
}

/// Fetch the lifetime download total for `app_id` from `/stats/<id>`.
/// Used to estimate a popularity-driven star rating for live entries.
pub fn downloads(app_id: &str) -> Option<u64> {
    let url = format!("{}/stats/{}", flathub_api_base(), app_id);
    let resp = http_get(&url)?;
    let json: serde_json::Value = serde_json::from_str(&resp).ok()?;
    json.get("installs_total").and_then(|v| v.as_u64())
}

// ---- JSON → model mapping -----------------------------------------

/// Map one Flathub search/collection hit object into an [`AppEntry`].
fn entry_from_search_hit(hit: &serde_json::Value) -> Option<AppEntry> {
    let id = hit
        .get("app_id")
        .or_else(|| hit.get("id"))
        .and_then(|v| v.as_str())?
        .to_string();
    let name = hit
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(&id)
        .to_string();
    let summary = hit
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let developer = hit
        .get("developer_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let category = hit
        .get("main_categories")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .map(category_from_appstream)
        .unwrap_or(Category::Productivity);
    let downloads = hit.get("installs_last_month").and_then(|v| v.as_u64());

    let mut entry = AppEntry::flathub_stub(&id, &name, &developer, &summary, category);
    if let Some(d) = downloads {
        entry.rating = rating_from_downloads(d);
        entry.rating_count = (d / 12).min(9_999_999) as u32;
        entry.download_count = Some(d);
    }
    entry.monogram = monogram_for(&name);
    entry.icon_url = icon_url_from(hit);
    Some(entry)
}

/// Extract a usable icon URL from a Flathub hit / AppStream record. Flathub
/// exposes the icon under `icon` (a CDN URL); some payloads instead carry an
/// `icons` array of `{ url | src }`. Only absolute http(s) URLs are kept.
fn icon_url_from(v: &serde_json::Value) -> Option<String> {
    if let Some(s) = v.get("icon").and_then(|i| i.as_str()) {
        if s.starts_with("http") {
            return Some(s.to_string());
        }
    }
    if let Some(arr) = v.get("icons").and_then(|i| i.as_array()) {
        for ic in arr {
            let cand = ic
                .get("url")
                .or_else(|| ic.get("src"))
                .and_then(|u| u.as_str());
            if let Some(s) = cand {
                if s.starts_with("http") {
                    return Some(s.to_string());
                }
            }
        }
    }
    None
}

/// Extract the best screenshot URL from a Flathub AppStream screenshot
/// object. Prefers a mid-size thumbnail (`thumb_url`), then the largest
/// entry in `sizes`, then a bare `source_url`/`src`. Only http(s) URLs.
fn screenshot_url_from(s: &serde_json::Value) -> Option<String> {
    let keep = |u: &str| u.starts_with("http").then(|| u.to_string());
    if let Some(u) = s.get("thumb_url").and_then(|v| v.as_str()).and_then(keep) {
        return Some(u);
    }
    if let Some(sizes) = s.get("sizes").and_then(|v| v.as_array()) {
        // Pick the widest size so the detail strip gets a crisp bitmap; the
        // ImageStore downsamples oversized sources anyway.
        let mut best: Option<(u64, String)> = None;
        for sz in sizes {
            let w = sz
                .get("width")
                .and_then(|w| w.as_u64().or_else(|| w.as_str().and_then(|s| s.parse().ok())))
                .unwrap_or(0);
            if let Some(src) = sz.get("src").and_then(|v| v.as_str()).and_then(keep) {
                if best.as_ref().map(|(bw, _)| w > *bw).unwrap_or(true) {
                    best = Some((w, src));
                }
            }
        }
        if let Some((_, src)) = best {
            return Some(src);
        }
    }
    s.get("source_url")
        .or_else(|| s.get("src"))
        .and_then(|v| v.as_str())
        .and_then(keep)
}

/// Map a full AppStream record into a detailed [`AppEntry`].
fn entry_from_appstream(app_id: &str, json: &serde_json::Value) -> Option<AppEntry> {
    let name = json.get("name").and_then(|v| v.as_str()).unwrap_or(app_id).to_string();
    let summary = json.get("summary").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let developer = json
        .get("developer_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let description = json
        .get("description")
        .and_then(|v| v.as_str())
        .map(strip_html)
        .unwrap_or_default();
    let category = json
        .get("categories")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .map(category_from_appstream)
        .unwrap_or(Category::Productivity);

    let mut entry = AppEntry::flathub_stub(app_id, &name, &developer, &summary, category);
    if !description.is_empty() {
        entry.description = description;
    }
    entry.monogram = monogram_for(&name);
    entry.icon_url = icon_url_from(json);

    // Version + size from the most recent release / bundle.
    if let Some(rel) = json
        .get("releases")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
    {
        if let Some(v) = rel.get("version").and_then(|v| v.as_str()) {
            entry.latest_version = v.to_string();
        }
    }
    if let Some(sz) = json
        .get("bundle")
        .and_then(|b| b.get("installed_size"))
        .and_then(|v| v.as_u64())
    {
        entry.size_mib = sz as f64 / (1024.0 * 1024.0);
    }

    // Screenshots → real bitmaps: carry the CDN URL so the detail page
    // fetches + decodes the actual screenshot (with a duotone fallback tone
    // pair should the fetch fail, so it is never a blank grey bar).
    if let Some(shots) = json.get("screenshots").and_then(|v| v.as_array()) {
        entry.shots = shots
            .iter()
            .take(6)
            .enumerate()
            .map(|(i, s)| {
                let caption = s
                    .get("caption")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Screenshot")
                    .to_string();
                Shot {
                    tone_a: 4 + i % 5,
                    tone_b: 1 + i % 4,
                    caption,
                    url: screenshot_url_from(s),
                }
            })
            .collect();
    }

    // Permissions from the AppStream `metadata`/`permissions` block, if any.
    if let Some(perms) = json.get("permissions").and_then(|v| v.as_array()) {
        entry.permissions = perms
            .iter()
            .filter_map(|p| p.as_str())
            .map(permission_from_appstream)
            .collect();
    }

    Some(entry)
}

/// Strip a minimal set of HTML tags out of an AppStream description so it
/// renders as plain wrapped paragraphs. Not a full parser — turns `</p>`
/// and `<br>` into paragraph breaks and drops the rest of the tags.
fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    let lowered = s.to_lowercase();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '<' {
            // Emit a paragraph break for block-closing tags.
            if lowered[i..].starts_with("</p>") || lowered[i..].starts_with("<br") {
                out.push_str("\n\n");
            }
            in_tag = true;
        } else if c == '>' {
            in_tag = false;
        } else if !in_tag {
            out.push(c);
        }
        i += 1;
    }
    // Collapse runs of >2 newlines and trim.
    let mut collapsed = String::new();
    let mut nl = 0;
    for ch in out.chars() {
        if ch == '\n' {
            nl += 1;
            if nl <= 2 {
                collapsed.push(ch);
            }
        } else {
            nl = 0;
            collapsed.push(ch);
        }
    }
    collapsed.trim().to_string()
}

/// Map an AppStream category id to our coarse browse [`Category`].
fn category_from_appstream(c: &str) -> Category {
    match c.to_lowercase().as_str() {
        "graphics" | "photography" | "art" => Category::Graphics,
        "development" | "developertools" | "ide" => Category::Development,
        "game" | "games" => Category::Games,
        "network" | "internet" | "webbrowser" | "email" | "chat" => Category::Internet,
        _ => Category::Productivity,
    }
}

/// Map an AppStream permission token (e.g. `network`, `filesystem=home`) to
/// a human-readable [`Permission`], flagging the sensitive ones.
fn permission_from_appstream(p: &str) -> Permission {
    let lp = p.to_lowercase();
    if lp.contains("network") {
        Permission::sensitive("Network access")
    } else if lp.contains("filesystem=home") || lp.contains("filesystem=host") {
        Permission::sensitive("Full home filesystem access")
    } else if lp.contains("devices") {
        Permission::sensitive("Device access")
    } else if lp.contains("audio") || lp.contains("pulseaudio") {
        Permission::normal("Audio playback")
    } else if lp.contains("wayland") || lp.contains("x11") {
        Permission::normal("Windowing")
    } else {
        Permission::normal(p)
    }
}

/// Estimate a 0..=5 star rating from a monthly download count, on a log
/// curve so a 10× difference is roughly half a star. Bounded to 3.5..=4.9
/// so nothing looks artificially terrible (we have no real review data).
pub fn rating_from_downloads(monthly: u64) -> f32 {
    if monthly == 0 {
        return 4.0;
    }
    let log = (monthly as f32).log10(); // ~2 (hundreds) .. ~6 (millions)
    (3.5 + (log - 2.0) * 0.32).clamp(3.5, 4.9)
}

/// First letter (uppercased) plus, if the name is camel/multi-word, a
/// second character — a compact monogram for the icon tile.
fn monogram_for(name: &str) -> String {
    let mut chars = name.chars().filter(|c| c.is_alphanumeric());
    let first = chars.next().unwrap_or('?').to_uppercase().to_string();
    // Second char from a word boundary if present.
    if let Some(rest) = name.split(|c: char| c == ' ' || c == '-' || c == '.').nth(1) {
        if let Some(c2) = rest.chars().next() {
            return format!("{first}{}", c2.to_lowercase());
        }
    }
    first
}

/// Reviews are not exposed by Flathub. For live entries we surface the
/// download count as the social-proof signal and synthesise no fake review
/// text; the sample catalog carries hand-written [`Review`]s instead. This
/// helper documents that decision at the call site.
pub fn live_reviews_unavailable() -> Vec<Review> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_binary_times_out_to_none() {
        let out = run_with_timeout(
            "definitely-not-a-real-binary-xyz",
            &["--version"],
            Duration::from_millis(500),
        );
        assert!(out.is_none());
    }

    #[test]
    fn rating_curve_is_bounded_and_monotonic() {
        let low = rating_from_downloads(100);
        let mid = rating_from_downloads(50_000);
        let high = rating_from_downloads(5_000_000);
        assert!((3.5..=4.9).contains(&low));
        assert!((3.5..=4.9).contains(&high));
        assert!(low <= mid && mid <= high, "rating must rise with downloads");
    }

    #[test]
    fn strip_html_makes_paragraphs() {
        let html = "<p>First para.</p><p>Second para.</p>";
        let out = strip_html(html);
        assert!(out.contains("First para."));
        assert!(out.contains("Second para."));
        assert!(out.contains("\n\n"), "paragraph break preserved");
        assert!(!out.contains('<'), "tags stripped");
    }

    #[test]
    fn category_mapping_covers_known_ids() {
        assert_eq!(category_from_appstream("Graphics"), Category::Graphics);
        assert_eq!(category_from_appstream("Network"), Category::Internet);
        assert_eq!(category_from_appstream("Game"), Category::Games);
        assert_eq!(category_from_appstream("Development"), Category::Development);
        assert_eq!(category_from_appstream("Office"), Category::Productivity);
    }

    #[test]
    fn permission_flags_sensitive_tokens() {
        assert!(permission_from_appstream("network").sensitive);
        assert!(permission_from_appstream("filesystem=home").sensitive);
        assert!(!permission_from_appstream("wayland").sensitive);
    }

    #[test]
    fn search_hit_maps_to_entry() {
        let hit = serde_json::json!({
            "app_id": "org.example.App",
            "name": "Example App",
            "summary": "Does things",
            "developer_name": "Example Inc.",
            "main_categories": ["Graphics"],
            "installs_last_month": 50000u64,
        });
        let entry = entry_from_search_hit(&hit).unwrap();
        assert_eq!(entry.id, "org.example.App");
        assert_eq!(entry.name, "Example App");
        assert_eq!(entry.category, Category::Graphics);
        assert_eq!(entry.download_count, Some(50000));
        assert!(entry.rating > 3.5);
    }

    #[test]
    fn monogram_picks_two_chars_for_multiword() {
        assert_eq!(monogram_for("Podman Desktop"), "Pd");
        assert_eq!(monogram_for("GIMP"), "G");
    }

    #[test]
    fn search_hit_carries_the_real_icon_url() {
        let hit = serde_json::json!({
            "app_id": "org.gimp.GIMP",
            "name": "GIMP",
            "summary": "Image editor",
            "icon": "https://dl.flathub.org/media/org/gimp/GIMP/icon.png",
        });
        let entry = entry_from_search_hit(&hit).unwrap();
        assert_eq!(
            entry.icon_url.as_deref(),
            Some("https://dl.flathub.org/media/org/gimp/GIMP/icon.png"),
            "a real icon URL must flow through so the tile shows the app's icon, not a letter"
        );
    }

    #[test]
    fn appstream_screenshots_carry_real_bitmap_urls() {
        let json = serde_json::json!({
            "name": "GIMP",
            "summary": "Image editor",
            "icon": "https://dl.flathub.org/media/org/gimp/GIMP/icon.png",
            "screenshots": [
                {
                    "caption": "Single-window mode",
                    "sizes": [
                        {"width": "224", "height": "126", "src": "https://dl.flathub.org/media/small.png"},
                        {"width": "752", "height": "423", "src": "https://dl.flathub.org/media/large.png"}
                    ]
                }
            ]
        });
        let entry = entry_from_appstream("org.gimp.GIMP", &json).unwrap();
        assert!(entry.icon_url.is_some());
        assert_eq!(entry.shots.len(), 1);
        // The widest size is chosen so the strip gets a crisp bitmap.
        assert_eq!(entry.shots[0].url.as_deref(), Some("https://dl.flathub.org/media/large.png"));
        assert_eq!(entry.shots[0].caption, "Single-window mode");
    }

    #[test]
    fn non_http_icon_is_rejected() {
        // A relative or data icon must not become a fetch URL.
        let hit = serde_json::json!({ "app_id": "a.B", "name": "B", "icon": "icons/hicolor/B.png" });
        assert!(entry_from_search_hit(&hit).unwrap().icon_url.is_none());
    }

    #[test]
    fn parse_installed_reads_flatpak_list_columns() {
        // The exact tab-separated shape `flatpak list --columns=...` emits.
        let out = "org.gimp.GIMP\t3.0.2\norg.mozilla.firefox\t129.0\n\t\n";
        let map = parse_installed(out);
        assert_eq!(map.get("org.gimp.GIMP").map(String::as_str), Some("3.0.2"));
        assert_eq!(map.get("org.mozilla.firefox").map(String::as_str), Some("129.0"));
        assert_eq!(map.len(), 2, "blank lines are skipped");
    }

    #[test]
    fn parse_remotes_reads_flatpak_remotes_columns() {
        let out = "flathub\thttps://dl.flathub.org/repo/\tFlathub\t\n\
                   dev\thttps://example.test/repo/\t\tdisabled\n";
        let remotes = parse_remotes(out);
        assert_eq!(remotes.len(), 2);
        assert_eq!(remotes[0].name, "flathub");
        assert_eq!(remotes[0].title, "Flathub");
        assert!(!remotes[0].disabled);
        // A remote with no title falls back to its name; `disabled` is parsed.
        assert_eq!(remotes[1].title, "dev");
        assert!(remotes[1].disabled);
    }

    #[test]
    fn install_args_are_noninteractive_and_pin_the_installation() {
        // The core fix: a bare `flatpak install flathub <ref>` is ambiguous
        // when flathub is in both installations. The argv MUST pin the
        // installation (`--user`/`--system`), stay non-interactive, and name
        // the remote — so flatpak resolves to exactly one source.
        let args = install_args(Scope::User, "flathub", "org.gimp.GIMP");
        assert_eq!(args, vec!["install", "--user", "--noninteractive", "flathub", "org.gimp.GIMP"]);
        assert!(args.iter().any(|a| a == "--user"), "installation is pinned");
        assert!(args.iter().any(|a| a == "--noninteractive"), "runs non-interactively");
        assert!(args.iter().any(|a| a == "flathub"), "remote is named explicitly");
        // The ambiguous bare form (`install flathub <ref>` with no scope flag)
        // is never produced.
        assert!(
            args.iter().any(|a| a == "--user" || a == "--system"),
            "an installation flag is always present so the remote is unambiguous",
        );

        let sys = install_args(Scope::System, "flathub", "a.b.C");
        assert_eq!(sys[1], "--system");
        assert_eq!(update_args(Scope::User, "a.b.C"), vec!["update", "--user", "--noninteractive", "a.b.C"]);
        assert_eq!(uninstall_args(Scope::System, "a.b.C"), vec!["uninstall", "--system", "--noninteractive", "a.b.C"]);
    }

    #[test]
    fn resolve_install_scope_prefers_user_then_system() {
        let remotes = vec![
            Remote { name: "flathub".into(), url: "u".into(), title: "Flathub".into(), disabled: false, scope: Some(Scope::User) },
            Remote { name: "flathub".into(), url: "u".into(), title: "Flathub".into(), disabled: false, scope: Some(Scope::System) },
        ];
        // Flathub in BOTH → prefer the user installation (no privilege prompt).
        assert_eq!(resolve_install_scope("flathub", &remotes), Scope::User);
        // Only system has it → system.
        let sys_only = vec![Remote { name: "fedora".into(), url: "u".into(), title: "Fedora".into(), disabled: false, scope: Some(Scope::System) }];
        assert_eq!(resolve_install_scope("fedora", &sys_only), Scope::System);
        // Unknown remote → default to user.
        assert_eq!(resolve_install_scope("nope", &[]), Scope::User);
    }

    #[test]
    fn parse_installed_apps_reads_all_columns_and_tags_scope() {
        let out = "org.gimp.GIMP\tGIMP\t3.0.2\tflathub\n\
                   org.mozilla.firefox\tFirefox\t129.0\tflathub\n\t\t\t\n";
        let apps = parse_installed_apps(out, Scope::System);
        assert_eq!(apps.len(), 2, "blank lines skipped");
        assert_eq!(apps[0].id, "org.gimp.GIMP");
        assert_eq!(apps[0].name, "GIMP");
        assert_eq!(apps[0].version, "3.0.2");
        assert_eq!(apps[0].origin, "flathub");
        assert_eq!(apps[0].scope, Scope::System);
    }

    #[test]
    fn parse_progress_reads_real_percent_and_phase() {
        // A `\r`-updated line's most recent percent wins.
        assert_eq!(parse_progress_fraction("Downloading… 12% 47%"), Some(0.47));
        assert_eq!(parse_progress_fraction("Installing 1/1… 100%"), Some(1.0));
        assert_eq!(parse_progress_fraction("no percent here"), None);
        // Out-of-range tokens are ignored.
        assert_eq!(parse_progress_fraction("999%"), None);

        assert_eq!(parse_progress_phase("Downloading org.gimp.GIMP"), Some(InstallPhase::Downloading));
        assert_eq!(parse_progress_phase("Installing 1/1"), Some(InstallPhase::Installing));
        assert_eq!(parse_progress_phase("idle"), None);
    }

    #[test]
    fn streaming_action_reports_real_progress_then_success() {
        // Drive the REAL streaming/parse/outcome path with a stand-in command
        // that emits flatpak-shaped progress and exits 0. Proves the UI would
        // see live fractions and a genuine success — not a wall-clock fake.
        let script = "printf 'Downloading 40%%\\r'; printf 'Installing 100%%\\n'; exit 0";
        let mut samples: Vec<ActionProgress> = Vec::new();
        let outcome = run_program_streaming(
            "sh",
            &["-c".into(), script.into()],
            |p| samples.push(p),
        );
        assert_eq!(outcome, ActionOutcome::Ok, "a zero exit is a real success");
        assert!(
            samples.iter().any(|s| s.fraction == Some(0.4)),
            "the 40% download sample surfaced: {samples:?}",
        );
        assert!(
            samples.iter().any(|s| s.fraction == Some(1.0) && s.phase == Some(InstallPhase::Installing)),
            "the 100% install sample surfaced: {samples:?}",
        );
    }

    #[test]
    fn streaming_action_surfaces_a_real_failure_reason() {
        // A non-zero exit is a REAL failure carrying the stderr reason — the
        // store must show FAILED, never keep filling a fake bar to 100%.
        let script = "echo 'error: No such ref in remote flathub' 1>&2; exit 1";
        let outcome = run_program_streaming("sh", &["-c".into(), script.into()], |_| {});
        match outcome {
            ActionOutcome::Failed(reason) => {
                assert!(reason.contains("No such ref"), "real reason surfaced: {reason}");
            }
            ActionOutcome::Ok => panic!("a non-zero exit must be Failed, not Ok"),
        }
    }

    #[test]
    fn failure_reason_extracts_last_line_and_flags_ambiguity() {
        // The real error from the bug report is recognised and reworded into
        // an actionable message rather than dumped raw.
        let amb = "error: Remote 'flathub' found in multiple installations, unable to proceed in non-interactive mode";
        assert!(is_multiple_installations_error(amb));
        assert!(failure_reason(amb).contains("multiple installations"));
        // A generic failure surfaces its last meaningful line.
        let generic = "Looking for matches…\nerror: No such ref 'app/x.Y/x86_64/stable' in remote flathub\n";
        assert_eq!(failure_reason(generic), "error: No such ref 'app/x.Y/x86_64/stable' in remote flathub");
        assert_eq!(failure_reason(""), "flatpak failed");
    }
}
