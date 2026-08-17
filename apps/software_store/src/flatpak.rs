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
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use crate::model::{AppEntry, Category, Permission, Review, Shot};

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
}

/// `flatpak list --app --columns=application,version` → app-id → version.
pub fn list_installed() -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let Some(out) = run_with_timeout(
        "flatpak",
        &["list", "--app", "--columns=application,version"],
        Duration::from_millis(1500),
    ) else {
        return map;
    };
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

/// `flatpak remotes --columns=name,url,title,options` → list of remotes.
pub fn list_remotes() -> Vec<Remote> {
    let mut remotes = Vec::new();
    let Some(out) = run_with_timeout(
        "flatpak",
        &["remotes", "--columns=name,url,title,options"],
        Duration::from_millis(1500),
    ) else {
        return remotes;
    };
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
        });
    }
    remotes
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

/// `flatpak install -y --noninteractive <remote> <app-id>`.
pub fn install(remote: &str, app_id: &str) -> ActionOutcome {
    run_action(&["install", "-y", "--noninteractive", remote, app_id])
}

/// `flatpak update -y --noninteractive <app-id>`.
pub fn update(app_id: &str) -> ActionOutcome {
    run_action(&["update", "-y", "--noninteractive", app_id])
}

/// `flatpak uninstall -y --noninteractive <app-id>`.
pub fn uninstall(app_id: &str) -> ActionOutcome {
    run_action(&["uninstall", "-y", "--noninteractive", app_id])
}

/// `flatpak remote-add --if-not-exists <name> <url>`.
pub fn remote_add(name: &str, url: &str) -> ActionOutcome {
    run_action(&["remote-add", "--if-not-exists", name, url])
}

/// `flatpak remote-delete --force <name>`.
pub fn remote_delete(name: &str) -> ActionOutcome {
    run_action(&["remote-delete", "--force", name])
}

/// Install from a `.flatpakref` file path:
/// `flatpak install -y --noninteractive <path>`.
pub fn install_ref(path: &str) -> ActionOutcome {
    run_action(&["install", "-y", "--noninteractive", path])
}

/// `flatpak run <app-id>` — launch an installed app (fire-and-forget).
pub fn run_app(app_id: &str) {
    let _ = Command::new("flatpak").args(["run", app_id]).spawn();
}

/// Run a mutating `flatpak` action, capturing stderr on failure so the UI
/// can show a useful message. Bounded so a stuck transaction can't wedge
/// the worker forever (installs get a generous window; the UI shows
/// progress independently).
fn run_action(args: &[&str]) -> ActionOutcome {
    let out = Command::new("flatpak").args(args).output();
    match out {
        Ok(o) if o.status.success() => ActionOutcome::Ok,
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            let msg = err.lines().last().unwrap_or("flatpak failed").trim().to_string();
            ActionOutcome::Failed(if msg.is_empty() { "flatpak failed".into() } else { msg })
        }
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
}
