//! A ratchet on what a Rust app still does for itself, which the document
//! host now does (`docs/architecture/apps.md` §8).
//!
//! Today every native app under `apps/` that uses the SDK runner loads the
//! identity broker the launcher passed it, obtains a signed join proof, asks
//! for content keys, and opens, signs and syncs its records in its own
//! process. All of that moves to the session's document host: an app's process
//! holds no identity, keys or transport, gets readings over the one
//! launcher-made connection, and submits actions over it.
//!
//! The migration is once, across every app, and it lands in slices, so this
//! test lists the apps that still carry app-side storage, identity or sync and
//! **fails if the list grows**. The list shrinks as slices land, and is empty
//! when the migration is done: a new app, or an added use, that has any of it
//! fails here rather than arriving unnoticed.
//!
//! What it counts, per app, from that app's own sources (never the SDK's):
//!
//! - `broker`: the app loads the identity broker or a collaboration client
//!   itself — `CollaborationManager`, `join_native`, `sdk_join_identity`,
//!   `NativeJoin`, `CollaborationManager::for_record` — including through
//!   `ui::collaboration::`.
//! - `keys`: the app asks for content keys or holds the device's channel-key
//!   service: `channel_keys`, `ChannelKeyProvider`, `install_channel_keys`.
//!   An app that provisions identity for a *system* (Imager enrolling a
//!   device, Settings publishing an identity) is not an app keeping its own
//!   records and is not counted.
//! - `storage`: the app opens, keeps or syncs its own records rather than a
//!   reading: `DocStore`, `DocOwner`, `app_state::`,
//!   `open_workspace_record`.
//!
//! `RecordStore` is deliberately not a marker: it names the SDK's read-only
//! handle to a record *another* process keeps, which is how a client of the
//! document host reads; what is counted is opening a store (`DocStore`) or
//! acquiring a document (`DocOwner`), which only the host may do.
//!
//! The ratchet is a plain list read from `tests/baselines/app-side-records.txt`
//! beside this test, so a reviewer reads what remains rather than a count. It
//! may only lose lines: running the test with `UPDATE_APP_SIDE_RECORDS=1`
//! rewrites the list, refusing to add a line unless
//! `ALLOW_APP_SIDE_RECORDS_GROWTH=1` is set, the way the UI ratchets refuse a
//! raised count.

#![cfg(target_os = "linux")]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Where the list of apps still on the old way is kept.
const BASELINE: &str = "tests/baselines/app-side-records.txt";

/// Set to rewrite the list from the tree; refuses to grow it without
/// [`GROWTH_VAR`].
const UPDATE_VAR: &str = "UPDATE_APP_SIDE_RECORDS";
/// Set with [`UPDATE_VAR`] to let a rewrite add a line. Review the diff first.
const GROWTH_VAR: &str = "ALLOW_APP_SIDE_RECORDS_GROWTH";

/// One way an app still does for itself what the document host does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Binges {
    /// It loads the identity broker or a collaboration client itself.
    Broker,
    /// It asks for content keys or holds an identity service.
    Keys,
    /// It opens, keeps or syncs its own records.
    Storage,
}

impl Binges {
    /// Its name in the list, and what a fix is.
    fn name(self) -> &'static str {
        match self {
            Self::Broker => "broker",
            Self::Keys => "keys",
            Self::Storage => "storage",
        }
    }

    /// The markers that mean `self`, in an app's own sources.
    fn markers(self) -> &'static [&'static str] {
        match self {
            Self::Broker => &[
                "CollaborationManager",
                "join_native",
                "sdk_join_identity",
                "NativeJoin",
                "CollaborationManager::for_record",
            ],
            Self::Keys => &["channel_keys", "ChannelKeyProvider", "install_channel_keys"],
            Self::Storage => &[
                "DocStore",
                "DocOwner",
                "app_state::",
                "open_workspace_record",
            ],
        }
    }
}

/// The repository root: this test's manifest is `<root>/crates/session/session`.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("the test runs from crates/session/session/Cargo.toml")
        .to_path_buf()
}

/// Every app under `apps/`, by its directory name, whether or not it has a
/// manifest of its own (a model crate beside its app has one).
fn apps(root: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root.join("apps")) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && path.join("Cargo.toml").is_file() {
            if let Some(name) = path.file_name().and_then(std::ffi::OsStr::to_str) {
                out.push((name.to_owned(), path));
            }
        }
    }
    out.sort();
    out
}

/// The `.rs` and `.toml` files under `dir`, without following links.
fn sources(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Build output and vendored copies are not the app's sources.
                let name = path.file_name().and_then(std::ffi::OsStr::to_str);
                if matches!(name, Some("target" | ".git")) {
                    continue;
                }
                stack.push(path);
            } else if matches!(
                path.extension().and_then(std::ffi::OsStr::to_str),
                Some("rs" | "toml")
            ) {
                out.push(path);
            }
        }
    }
    out
}

/// What `app` still does for itself, as the slices name it.
fn carries(app: &Path) -> BTreeSet<Binges> {
    let mut found = BTreeSet::new();
    let mut text = String::new();
    for file in sources(app) {
        let Ok(read) = std::fs::read_to_string(&file) else {
            continue;
        };
        text.push_str(&read);
        text.push('\n');
    }
    for what in [Binges::Broker, Binges::Keys, Binges::Storage] {
        if what.markers().iter().any(|marker| text.contains(marker)) {
            found.insert(what);
        }
    }
    found
}

/// The list as it is now: `app: what, what`, one app per line, sorted.
fn current(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for (name, path) in apps(root) {
        let carried = carries(&path);
        if carried.is_empty() {
            continue;
        }
        let names: Vec<&str> = carried.iter().map(|what| what.name()).collect();
        out.push(format!("{name}: {}", names.join(", ")));
    }
    out
}

/// The lines the baseline keeps, ignoring blank lines and comments.
fn kept(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

/// The app a line names.
fn app_of(line: &str) -> &str {
    line.split_once(':').map_or(line, |(app, _)| app).trim()
}

#[test]
fn apps_using_app_side_storage_identity_or_sync_do_not_grow() {
    let root = root();
    let baseline = root.join("crates/session/session").join(BASELINE);
    let now = current(&root);
    let before = kept(&baseline);

    if std::env::var_os(UPDATE_VAR).is_some() {
        let grew = now
            .iter()
            .any(|line| !before.iter().any(|kept| app_of(kept) == app_of(line)));
        assert!(
            !grew || std::env::var_os(GROWTH_VAR).is_some(),
            "this rewrite would add an app to the list; an app does not take up app-side \
             storage, identity or sync again — review, then set {GROWTH_VAR} if it is right"
        );
        let body = format!(
            "# Apps still doing for themselves what the document host now does\n\
             # (docs/architecture/apps.md §8). One line per app, as `app: what`.\n\
             # This list may only shrink: a slice that migrates an app deletes its\n\
             # line, and a new app, or an added use, fails the ratchet.\n\n{}\n",
            now.join("\n")
        );
        std::fs::write(&baseline, body).expect("the baseline writes");
        eprintln!("rewrote {BASELINE}: {} apps", now.len());
        return;
    }

    // Growth: an app present now, or a use now, that the baseline does not
    // name. Judged per app, so a migrated app whose line lost a use is a
    // drop, never a growth.
    let mut growth = Vec::new();
    for line in &now {
        match before.iter().find(|kept| app_of(kept) == app_of(line)) {
            None => growth.push(line.clone()),
            Some(kept_line) => {
                let was: BTreeSet<&str> = kept_line
                    .split_once(':')
                    .map(|(_, what)| what)
                    .unwrap_or_default()
                    .split(',')
                    .map(str::trim)
                    .filter(|what| !what.is_empty())
                    .collect();
                let is: BTreeSet<&str> = line
                    .split_once(':')
                    .map(|(_, what)| what)
                    .unwrap_or_default()
                    .split(',')
                    .map(str::trim)
                    .filter(|what| !what.is_empty())
                    .collect();
                let added: Vec<&&str> = is.difference(&was).collect();
                if !added.is_empty() {
                    growth.push(format!("{}: {added:?} added", app_of(line)));
                }
            }
        }
    }
    assert!(
        growth.is_empty(),
        "apps took up app-side storage, identity or sync, which the document host does \
         (docs/architecture/apps.md §8): {growth:#?}\n\
         An app holds no identity, keys or transport: it is handed readings and submits \
         actions over its one launcher-made connection. The host runs its model and verbs \
         (tilo-model-host), so an app's process needs none of what this lists."
    );

    // A drop is not a failure: it only means the baseline can lose the line.
    let stale: Vec<&String> = before
        .iter()
        .filter(|line| !now.iter().any(|now| app_of(now) == app_of(line)))
        .collect();
    if !stale.is_empty() {
        eprintln!(
            "{BASELINE} still names {} app(s) that no longer carry any of it; \
             run {UPDATE_VAR}=1 to tighten it:\n{}",
            stale.len(),
            stale
                .iter()
                .map(|line| format!("  - {line}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}
