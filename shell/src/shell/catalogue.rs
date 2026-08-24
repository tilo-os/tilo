//! Apps the machine can open but has not installed yet.
//!
//! [`crate::shell::launcher`] enumerates what is *here*: the packages
//! `qos-install` has unpacked and recorded. This module is the other
//! list — what the user's configured registries offer that is not here
//! — and the path that turns one of those into a running app.
//!
//! ## Nothing here does HTTP
//!
//! Both halves shell out to `qos-catalogue`, exactly as launching
//! shells out to `qos-launcher`. Listing means a network round-trip
//! and installing additionally means signature verification and
//! archive extraction; none of that belongs inside the compositor
//! process, and keeping it out means a hostile registry response
//! cannot reach the code that owns every window on the machine.
//!
//! ## Never on the frame thread
//!
//! [`available_apps`] answers from an in-memory cache and returns
//! immediately, even when the cache is empty or stale — the palette
//! calls it on every keystroke. A stale cache schedules a refresh on a
//! worker thread and answers with what it has. An empty catalogue is a
//! normal, correct answer: a machine with no registries configured
//! genuinely knows about no other software.
//!
//! ## Opening, not installing
//!
//! [`install_and_launch`] is the whole of what activating an
//! uninstalled app does: fetch it, then start it through the *same*
//! [`crate::shell::launcher::launch`] every installed app goes
//! through. There is no separate "installed app" launch path, so an
//! app opened this way is sandboxed, roomed and placed identically to
//! one that was already here — the install is a step on the way to
//! opening, not a different kind of event.
//!
//! ## Saying how far along it is
//!
//! That step takes seconds, and seconds of silence are
//! indistinguishable from an Enter key that did nothing. So the fetch
//! is watched rather than waited on: `qos-catalogue install` narrates
//! on stdout as bytes arrive, this module parses that narration into a
//! [`FetchPhase`], and the frame thread reads the phase to word the
//! notice and size its bar. The phase is public state, not a private
//! detail of the toast — an agent driving the palette over the control
//! socket cannot see a toast, and has the same right to know whether
//! the thing it asked for is still coming.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::shell::launcher::{self, LaunchEvent, LaunchEventKind};
use crate::shell::state::{CapOverride, RoomId};

/// How long a fetched catalogue is reused before a refresh is
/// scheduled.
///
/// Much longer than the installed-apps TTL: that one re-reads two
/// local files, this one talks to a network service, and the set of
/// software a registry offers does not change between keystrokes.
const CATALOGUE_TTL: Duration = Duration::from_secs(120);

/// How long a failed refresh is remembered before another is tried.
/// Without this a machine configured with an unreachable registry
/// would start a fresh HTTP attempt every time the palette opened.
const FAILED_REFRESH_BACKOFF: Duration = Duration::from_secs(30);

/// One app a registry offers that is not installed here.
///
/// **Wire contract.** These field names are what `qos-catalogue list
/// --json` prints; the crate that prints them pins the same names in
/// its own test. Changing one without the other silently produces an
/// empty catalogue rather than an error, so both halves have a test
/// that would have to be edited together.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CatalogueApp {
    pub package_id: String,
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub category: Option<String>,
    pub version: String,
    /// Which configured registry offered it.
    pub registry: String,
    #[serde(default)]
    pub trust: String,
}

/// The envelope `qos-catalogue list --json` prints.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CatalogueListing {
    #[serde(default)]
    pub apps: Vec<CatalogueApp>,
    #[serde(default)]
    pub unreachable: Vec<UnreachableRegistry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct UnreachableRegistry {
    pub name: String,
    pub detail: String,
}

// ---------------------------------------------------------------------
// Process-wide state. One machine, one set of configured registries.
// ---------------------------------------------------------------------

static CATALOGUE: Mutex<Option<(Vec<CatalogueApp>, Instant)>> = Mutex::new(None);
/// Set while a refresh worker is in flight, so a burst of keystrokes
/// schedules one refresh rather than one per character.
static REFRESHING: AtomicBool = AtomicBool::new(false);
/// The notification raised for an in-flight open, per package, so the
/// frame thread can retire it when the app actually starts.
static PENDING_NOTICES: Mutex<Vec<(String, u64)>> = Mutex::new(Vec::new());
/// How far along each in-flight open is. Written by the worker thread
/// as `qos-catalogue` narrates, read by the frame thread to word the
/// notice and size its bar.
static FETCH_PHASES: Mutex<Vec<(String, FetchPhase)>> = Mutex::new(Vec::new());

// ---------------------------------------------------------------------
// How far along an open is.
// ---------------------------------------------------------------------

/// Where an in-flight open has got to.
///
/// This exists because an open that has to fetch takes seconds, and a
/// second of silence is indistinguishable from an Enter key that did
/// nothing. It is a *reported* state rather than a private one: the
/// toast reads it to word itself and size its bar, and a caller driving
/// the palette over the control socket — which cannot see the toast —
/// reads it to find out the same thing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchPhase {
    /// Asked for it; nothing has come back yet.
    Connecting,
    /// Bytes are arriving. `total` is the size the registry declared,
    /// when it declared one — without it there is no honest fraction to
    /// draw, only the fact that something is happening.
    Downloading { received: u64, total: Option<u64> },
    /// The bytes are here and are being checked and extracted. No
    /// fraction: the installer verifies and unpacks as one step, and a
    /// bar invented for it would be a bar that lies.
    Unpacking,
    /// Verified, unpacked, and being launched like any other app.
    Starting,
    /// It did not happen, and this is why. Kept rather than dropped so
    /// the end of an open is always something a caller can read, not an
    /// entry that quietly disappears.
    Failed { detail: String },
}

impl FetchPhase {
    /// The fraction to draw, when there is an honest one. `None` means
    /// indeterminate: something is happening and how much is left is
    /// genuinely unknown.
    pub fn fraction(&self) -> Option<f32> {
        match self {
            FetchPhase::Downloading {
                received,
                total: Some(total),
            } if *total > 0 => Some((*received as f64 / *total as f64).clamp(0.0, 1.0) as f32),
            _ => None,
        }
    }

    /// Whether this is a phase the open can still move on from.
    pub fn is_in_progress(&self) -> bool {
        !matches!(self, FetchPhase::Failed { .. })
    }
}

/// Record how far along `package_id` is. Called from the worker thread
/// and, for the opening state, from the frame thread at activation.
pub fn set_fetch_phase(package_id: &str, phase: FetchPhase) {
    let mut phases = FETCH_PHASES.lock().expect("FETCH_PHASES poisoned");
    match phases.iter_mut().find(|(id, _)| id == package_id) {
        Some(slot) => slot.1 = phase,
        None => phases.push((package_id.to_string(), phase)),
    }
}

/// How far along `package_id` is, if an open is outstanding for it.
pub fn fetch_phase(package_id: &str) -> Option<FetchPhase> {
    FETCH_PHASES
        .lock()
        .expect("FETCH_PHASES poisoned")
        .iter()
        .find(|(id, _)| id == package_id)
        .map(|(_, p)| p.clone())
}

/// Every open the machine currently has in flight, and how far along
/// each one is.
pub fn fetch_phases() -> Vec<(String, FetchPhase)> {
    FETCH_PHASES
        .lock()
        .expect("FETCH_PHASES poisoned")
        .clone()
}

/// Forget `package_id`'s phase. Called when the open settles — the app
/// is on screen, or the failure has been reported.
pub fn clear_fetch_phase(package_id: &str) {
    FETCH_PHASES
        .lock()
        .expect("FETCH_PHASES poisoned")
        .retain(|(id, _)| id != package_id);
}

/// Parse one narration line from `qos-catalogue install`.
///
/// Split out from the subprocess so the wire contract between the two
/// halves is assertable without a network, a registry, or a fetch. The
/// other half of the contract is the `install` command's own module
/// header; a rename would have to change both.
pub fn parse_progress_line(line: &str) -> Option<FetchPhase> {
    let mut parts = line.split_whitespace();
    match parts.next()? {
        "progress" => {
            let received: u64 = parts.next()?.parse().ok()?;
            let total = match parts.next()? {
                "-" => None,
                n => Some(n.parse().ok()?),
            };
            Some(FetchPhase::Downloading { received, total })
        }
        "phase" => match parts.next()? {
            "unpacking" => Some(FetchPhase::Unpacking),
            _ => None,
        },
        _ => None,
    }
}

/// Render a byte count the way a person reads one.
///
/// Deliberately coarse: this sits in a toast next to a moving bar, and
/// a third decimal place there is noise that changes every frame.
pub fn human_bytes(n: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let n = n as f64;
    if n >= GB {
        format!("{:.1} GB", n / GB)
    } else if n >= MB {
        format!("{:.1} MB", n / MB)
    } else if n >= KB {
        format!("{:.0} KB", n / KB)
    } else {
        format!("{n:.0} B")
    }
}

/// Where we look for `qos-catalogue`, in order — the same search the
/// launcher does for `qos-launcher`, for the same reasons.
fn catalogue_bin_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(explicit) = std::env::var_os("QOS_CATALOGUE_BIN") {
        if !explicit.is_empty() {
            out.push(PathBuf::from(explicit));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            out.push(dir.join("qos-catalogue"));
        }
    }
    out.push(PathBuf::from("/usr/libexec/qos-catalogue"));
    out.push(PathBuf::from("/usr/bin/qos-catalogue"));
    out
}

/// First candidate that exists.
///
/// `None` means this machine has no catalogue binary, which is not an
/// error: the desktop then offers exactly the installed apps, as it
/// did before this path existed.
pub fn resolve_catalogue_bin() -> Option<PathBuf> {
    catalogue_bin_candidates().into_iter().find(|p| p.exists())
}

/// Parse the `list --json` envelope.
///
/// Split out from the subprocess so the wire contract is assertable
/// without spawning anything.
pub fn parse_listing(stdout: &str) -> Result<CatalogueListing, String> {
    serde_json::from_str(stdout).map_err(|e| e.to_string())
}

/// Available-but-not-installed apps, from cache.
///
/// Returns immediately, always. A cold or stale cache schedules a
/// refresh and answers with whatever it currently holds — the palette
/// calls this on every keystroke and must never wait on a network.
pub fn available_apps() -> Vec<CatalogueApp> {
    let (apps, needs_refresh) = {
        let cache = CATALOGUE.lock().expect("CATALOGUE poisoned");
        match cache.as_ref() {
            Some((apps, at)) => (apps.clone(), at.elapsed() >= CATALOGUE_TTL),
            None => (Vec::new(), true),
        }
    };
    if needs_refresh {
        schedule_refresh();
    }
    apps
}

/// Drop the cached catalogue and force the next read to refetch.
/// Called after an install so the app that just arrived stops being
/// offered as available.
pub fn invalidate() {
    *CATALOGUE.lock().expect("CATALOGUE poisoned") = None;
}

/// Replace the cache directly. For tests, which must not depend on a
/// registry being reachable.
pub fn seed_for_test(apps: Vec<CatalogueApp>) {
    *CATALOGUE.lock().expect("CATALOGUE poisoned") = Some((apps, Instant::now()));
}

/// Run `qos-catalogue list --json` on a worker thread and cache the
/// result.
///
/// A failure caches an *empty* list rather than leaving the cache
/// cold: a cold cache would schedule another refresh on the very next
/// keystroke, so an unreachable registry would mean a new HTTP attempt
/// per character typed. The empty result expires under
/// [`FAILED_REFRESH_BACKOFF`], so a registry that comes back is picked
/// up shortly after.
fn schedule_refresh() {
    if REFRESHING.swap(true, Ordering::SeqCst) {
        return;
    }
    let Some(bin) = resolve_catalogue_bin() else {
        // Nothing to run. Cache an empty answer so we don't re-resolve
        // the binary on every keystroke.
        *CATALOGUE.lock().expect("CATALOGUE poisoned") = Some((Vec::new(), Instant::now()));
        REFRESHING.store(false, Ordering::SeqCst);
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("qos-catalogue-refresh".to_string())
        .spawn(move || {
            let out = std::process::Command::new(&bin)
                .args(["list", "--json"])
                .output();
            let (apps, expiry) = match out {
                Ok(o) if o.status.success() => {
                    let text = String::from_utf8_lossy(&o.stdout);
                    match parse_listing(&text) {
                        Ok(listing) => {
                            for bad in &listing.unreachable {
                                eprintln!(
                                    "[catalogue] registry {} unreachable: {}",
                                    bad.name, bad.detail
                                );
                            }
                            let fresh = listing.unreachable.is_empty();
                            (listing.apps, fresh)
                        }
                        Err(e) => {
                            eprintln!("[catalogue] could not parse the listing: {e}");
                            (Vec::new(), false)
                        }
                    }
                }
                Ok(o) => {
                    eprintln!(
                        "[catalogue] list failed ({}): {}",
                        o.status,
                        String::from_utf8_lossy(&o.stderr).trim()
                    );
                    (Vec::new(), false)
                }
                Err(e) => {
                    eprintln!("[catalogue] could not run {}: {e}", bin.display());
                    (Vec::new(), false)
                }
            };
            // A result we know is incomplete is backdated so it
            // expires sooner than a good one.
            let stamped = if expiry {
                Instant::now()
            } else {
                Instant::now()
                    .checked_sub(CATALOGUE_TTL.saturating_sub(FAILED_REFRESH_BACKOFF))
                    .unwrap_or_else(Instant::now)
            };
            *CATALOGUE.lock().expect("CATALOGUE poisoned") = Some((apps, stamped));
            REFRESHING.store(false, Ordering::SeqCst);
        });
    if spawned.is_err() {
        REFRESHING.store(false, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------
// Opening something that isn't here yet.
// ---------------------------------------------------------------------

/// Remember the notification raised for an in-flight open.
pub fn note_pending_notice(package_id: &str, notification_id: u64) {
    let mut pending = PENDING_NOTICES.lock().expect("PENDING_NOTICES poisoned");
    pending.retain(|(id, _)| id != package_id);
    pending.push((package_id.to_string(), notification_id));
}

/// The package ids with an open still outstanding, and the notice each
/// one raised.
///
/// A peek, not a take: the frame thread walks this every frame to ask
/// whether the window it is waiting for has appeared yet, and asking is
/// not settling. Retirement goes through [`take_pending_notice`].
pub fn pending_notices() -> Vec<(String, u64)> {
    PENDING_NOTICES
        .lock()
        .expect("PENDING_NOTICES poisoned")
        .clone()
}

/// Take the notification raised for `package_id`, if one is still
/// outstanding. Called on the frame thread when the launch resolves,
/// so the "opening" toast is retired by the app appearing rather than
/// by a timer.
pub fn take_pending_notice(package_id: &str) -> Option<u64> {
    let mut pending = PENDING_NOTICES.lock().expect("PENDING_NOTICES poisoned");
    let idx = pending.iter().position(|(id, _)| id == package_id)?;
    Some(pending.remove(idx).1)
}

/// Fetch `package_id`, then launch it into `room` — on a worker
/// thread, reporting through the launcher's own event queue.
///
/// `capabilities` is the activating workspace's grant list, captured
/// on the frame thread. It is republished immediately before the
/// launch rather than read fresh: an install takes seconds, and the
/// app must land in the sandbox of the workspace the user opened it
/// from, not whichever one they happen to be looking at when the
/// download finishes.
///
/// Everything this reports rides the same [`LaunchEvent`] queue a
/// normal launch uses, so a failure here surfaces through the same
/// path — and with the same wording — as any other failure to open an
/// app.
pub fn install_and_launch(
    package_id: String,
    display_name: String,
    room: RoomId,
    capabilities: Vec<CapOverride>,
) {
    // The first phase is set here rather than on the worker, so the
    // very next frame already has something true to show. A thread that
    // has not been scheduled yet is still an open in progress.
    set_fetch_phase(&package_id, FetchPhase::Connecting);
    let Some(bin) = resolve_catalogue_bin() else {
        push_failure(
            &package_id,
            &display_name,
            "this system has no app catalogue (qos-catalogue is missing)".to_string(),
        );
        return;
    };
    let builder = std::thread::Builder::new().name("qos-catalogue-install".to_string());
    let identity = (package_id.clone(), display_name.clone());
    let spawn_result = builder.spawn(move || {
        // Piped, not captured: the whole point is to know where the
        // fetch has got to *while* it is happening, and `output()`
        // only ever answers after it has stopped.
        let child = std::process::Command::new(&bin)
            .args(["install", &package_id])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match child {
            Err(e) => {
                push_failure(
                    &package_id,
                    &display_name,
                    format!("could not run the app catalogue: {e}"),
                );
                return;
            }
            Ok(c) => c,
        };
        if let Some(stdout) = child.stdout.take() {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(phase) = parse_progress_line(&line) {
                    set_fetch_phase(&package_id, phase);
                }
            }
        }
        // stdout closing means the child is done writing; `wait` then
        // returns without blocking on a pipe nobody is draining.
        let mut stderr_text = String::new();
        if let Some(mut err) = child.stderr.take() {
            use std::io::Read;
            let _ = err.read_to_string(&mut stderr_text);
        }
        match child.wait() {
            Err(e) => {
                push_failure(
                    &package_id,
                    &display_name,
                    format!("could not run the app catalogue: {e}"),
                );
                return;
            }
            Ok(status) if !status.success() => {
                let detail = stderr_text.trim().to_string();
                let detail = if detail.is_empty() {
                    format!("the app catalogue exited {status}")
                } else {
                    detail
                };
                push_failure(&package_id, &display_name, detail);
                return;
            }
            Ok(_) => {}
        }

        // The registry no longer offers it — it is here now.
        invalidate();
        // The installed-apps enumeration is memoised; the package that
        // just landed is younger than the memo.
        launcher::invalidate_app_cache();

        set_fetch_phase(&package_id, FetchPhase::Starting);
        launcher::set_workspace_capabilities(capabilities);
        if let Err(e) = launcher::launch(&package_id, room) {
            push_failure(&package_id, &display_name, e.user_message());
        }
    });
    if spawn_result.is_err() {
        push_failure(
            &identity.0,
            &identity.1,
            "could not start loading it".to_string(),
        );
    }
}

fn push_failure(package_id: &str, display_name: &str, detail: String) {
    // Recorded before it is pushed, so an open never ends in a state
    // that reads as "still going". A caller that polls the phase and a
    // person watching the toast learn the same thing at the same time.
    set_fetch_phase(
        package_id,
        FetchPhase::Failed {
            detail: detail.clone(),
        },
    );
    launcher::push_event(LaunchEvent {
        package_id: package_id.to_string(),
        display_name: display_name.to_string(),
        kind: LaunchEventKind::SpawnFailed { detail },
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire contract, pinned against the exact JSON the
    /// `qos-catalogue` binary prints. The other half of this pin lives
    /// in `app-catalogue`'s own test; the two would have to be edited
    /// together for a field rename to go unnoticed.
    #[test]
    fn the_listing_envelope_parses_the_binarys_field_names() {
        let json = r#"{"apps":[{"package_id":"dev.qos.sheets","display_name":"Sheets",
            "description":"a spreadsheet","category":"Office","version":"2.0.0",
            "registry":"main","download_url":"https://x/y.qpkg","trust":"verified"}],
            "unreachable":[{"name":"mirror","detail":"connection refused"}]}"#;
        let listing = parse_listing(json).expect("the envelope must parse");
        assert_eq!(listing.apps.len(), 1);
        let app = &listing.apps[0];
        assert_eq!(app.package_id, "dev.qos.sheets");
        assert_eq!(app.display_name, "Sheets");
        assert_eq!(app.category.as_deref(), Some("Office"));
        assert_eq!(app.version, "2.0.0");
        assert_eq!(app.registry, "main");
        assert_eq!(app.trust, "verified");
        assert_eq!(listing.unreachable.len(), 1);
        assert_eq!(listing.unreachable[0].name, "mirror");
    }

    /// A machine with nothing available produces an empty listing, not
    /// a parse error — that is the shape the binary prints when no
    /// registry is configured.
    #[test]
    fn an_empty_catalogue_is_a_valid_answer() {
        let listing = parse_listing(r#"{"apps":[],"unreachable":[]}"#).expect("parses");
        assert!(listing.apps.is_empty());
        let bare = parse_listing("{}").expect("a bare object parses too");
        assert!(bare.apps.is_empty());
    }

    #[test]
    fn garbage_on_stdout_is_an_error_not_an_empty_list() {
        assert!(
            parse_listing("not json at all").is_err(),
            "an unparseable listing must be reported, not silently read as \
             'nothing is available'"
        );
    }

    /// A pending notice belongs to exactly one open, and taking it
    /// consumes it — a second launch of the same app must not dismiss
    /// a notification that no longer exists.
    #[test]
    fn a_pending_notice_is_taken_once() {
        note_pending_notice("dev.qos.noticefixture", 42);
        assert_eq!(take_pending_notice("dev.qos.noticefixture"), Some(42));
        assert_eq!(take_pending_notice("dev.qos.noticefixture"), None);
    }

    #[test]
    fn a_newer_open_replaces_the_notice_for_the_same_package() {
        note_pending_notice("dev.qos.replacefixture", 1);
        note_pending_notice("dev.qos.replacefixture", 2);
        assert_eq!(
            take_pending_notice("dev.qos.replacefixture"),
            Some(2),
            "the live open's notification is the one to retire"
        );
        assert_eq!(take_pending_notice("dev.qos.replacefixture"), None);
    }

    // -----------------------------------------------------------------
    // How far along an open is.
    // -----------------------------------------------------------------

    /// The narration contract, pinned against the exact lines
    /// `qos-catalogue install` prints. The other half of this pin is
    /// that binary's module header.
    #[test]
    fn the_narration_parses_the_binarys_lines() {
        assert_eq!(
            parse_progress_line("progress 65536 1048576"),
            Some(FetchPhase::Downloading {
                received: 65536,
                total: Some(1048576),
            })
        );
        assert_eq!(
            parse_progress_line("progress 65536 -"),
            Some(FetchPhase::Downloading {
                received: 65536,
                total: None,
            }),
            "a registry that declares no size still reports arrival"
        );
        assert_eq!(
            parse_progress_line("phase unpacking"),
            Some(FetchPhase::Unpacking)
        );
        // Anything else on the pipe is the binary talking about
        // something this does not model — ignored, never mistaken for
        // a phase.
        assert_eq!(parse_progress_line("installed dev.qos.sheets 2.0.0"), None);
        assert_eq!(parse_progress_line(""), None);
        assert_eq!(parse_progress_line("progress notanumber 5"), None);
    }

    /// A fraction is only drawn when there is an honest one. Everything
    /// else is indeterminate — a bar for an unpack step whose duration
    /// nobody knows would be a bar that lies.
    #[test]
    fn only_a_sized_download_yields_a_fraction() {
        assert_eq!(
            FetchPhase::Downloading {
                received: 512,
                total: Some(2048)
            }
            .fraction(),
            Some(0.25)
        );
        assert_eq!(
            FetchPhase::Downloading {
                received: 512,
                total: None
            }
            .fraction(),
            None
        );
        assert_eq!(FetchPhase::Connecting.fraction(), None);
        assert_eq!(FetchPhase::Unpacking.fraction(), None);
        assert_eq!(FetchPhase::Starting.fraction(), None);
        // A server that declares zero cannot divide; it must not
        // produce a NaN-width bar either.
        assert_eq!(
            FetchPhase::Downloading {
                received: 0,
                total: Some(0)
            }
            .fraction(),
            None
        );
    }

    /// A failure is a terminal phase that stays readable, not an entry
    /// that disappears. A caller that cannot see the screen has to be
    /// able to tell "it failed" from "it is still going".
    #[test]
    fn a_failure_is_a_reported_phase_not_a_silence() {
        let id = "dev.qos.failfixture";
        clear_fetch_phase(id);
        set_fetch_phase(id, FetchPhase::Connecting);
        assert!(
            fetch_phase(id).expect("an open is outstanding").is_in_progress(),
            "an open that has started reads as in progress"
        );
        push_failure(id, "Fail Fixture", "the download failed: connection refused".into());
        let phase = fetch_phase(id).expect("a failed open still has a phase");
        assert_eq!(
            phase,
            FetchPhase::Failed {
                detail: "the download failed: connection refused".to_string()
            },
            "the reason has to survive to whoever is waiting"
        );
        assert!(
            !phase.is_in_progress(),
            "a failed open must not keep reading as still going"
        );
        clear_fetch_phase(id);
        assert_eq!(fetch_phase(id), None);
    }

    /// The phase moves forward as the narration arrives, so a reader
    /// polling it sees the open progress rather than one frozen value.
    #[test]
    fn narration_advances_the_reported_phase() {
        let id = "dev.qos.phasefixture";
        clear_fetch_phase(id);
        set_fetch_phase(id, FetchPhase::Connecting);
        for line in ["progress 0 2048", "progress 1024 2048", "phase unpacking"] {
            if let Some(p) = parse_progress_line(line) {
                set_fetch_phase(id, p);
            }
        }
        assert_eq!(fetch_phase(id), Some(FetchPhase::Unpacking));
        set_fetch_phase(id, FetchPhase::Starting);
        assert!(
            fetch_phases()
                .iter()
                .any(|(p, ph)| p == id && *ph == FetchPhase::Starting),
            "the whole in-flight set is readable, not just one id"
        );
        clear_fetch_phase(id);
    }

    #[test]
    fn byte_counts_read_the_way_a_person_reads_them() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2 KB");
        assert_eq!(human_bytes(3 * 1024 * 1024 + 512 * 1024), "3.5 MB");
        assert_eq!(human_bytes(2 * 1024 * 1024 * 1024), "2.0 GB");
    }

    /// Poll `f` until it answers `Some`, or give up. Fetches run on a
    /// worker thread against a real subprocess, so the test waits on
    /// the outcome rather than on a fixed duration that would be
    /// flaky on a loaded machine and slow on an idle one.
    fn wait_for<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(v) = f() {
                return v;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The whole reporting path, end to end through a real subprocess:
    /// activating an open moves through a visible in-progress state
    /// before anything is running, and a fetch that fails ends in a
    /// state that says why.
    ///
    /// Driven by a stand-in `qos-catalogue` rather than a seam in our
    /// own code, because what is being proved is that the two halves
    /// agree — the narration one prints and the phases the other
    /// parses. A test that called `set_fetch_phase` directly would pass
    /// with the pipe unplugged.
    #[test]
    #[serial_test::serial(process_env)]
    fn an_open_reports_progress_and_then_reports_why_it_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let release = tmp.path().join("release");
        let fake = tmp.path().join("qos-catalogue");
        // Narrates like the real binary, then holds until the test
        // releases it, then refuses. Holding is what makes the
        // in-progress state observable at all: a fetch that finished
        // instantly would never *be* in progress.
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\n\
                 echo 'progress 0 4096'\n\
                 echo 'progress 1024 4096'\n\
                 while [ ! -f '{}' ]; do sleep 0.02; done\n\
                 echo 'the download failed: connection refused' >&2\n\
                 exit 1\n",
                release.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let id = "dev.qos.progressfixture";
        clear_fetch_phase(id);
        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::set_var("QOS_CATALOGUE_BIN", &fake) };
        install_and_launch(
            id.to_string(),
            "Progress Fixture".to_string(),
            RoomId::nil(),
            Vec::new(),
        );

        // 1. It is in progress, and the progress is real: the byte
        //    counts came off the subprocess's own narration.
        let downloading = wait_for("the fetch to report bytes", || match fetch_phase(id) {
            Some(p @ FetchPhase::Downloading { .. }) => Some(p),
            _ => None,
        });
        assert_eq!(
            downloading,
            FetchPhase::Downloading {
                received: 1024,
                total: Some(4096),
            },
            "the phase carries what actually arrived, not a placeholder"
        );
        assert_eq!(
            downloading.fraction(),
            Some(0.25),
            "and a sized download gives an honest fraction to draw"
        );

        // 2. It fails legibly rather than falling silent.
        std::fs::write(&release, b"go").unwrap();
        let failed = wait_for("the fetch to report its failure", || match fetch_phase(id) {
            Some(p @ FetchPhase::Failed { .. }) => Some(p),
            _ => None,
        });
        let FetchPhase::Failed { detail } = &failed else {
            unreachable!("guarded by the wait")
        };
        assert!(
            detail.contains("connection refused"),
            "the reason has to reach whoever was waiting, not just a log: \
             {detail:?}"
        );
        assert!(
            !failed.is_in_progress(),
            "and it must stop reading as still going"
        );

        unsafe { std::env::remove_var("QOS_CATALOGUE_BIN") };
        clear_fetch_phase(id);
    }

    /// `QOS_CATALOGUE_BIN` is how a test — and the headless harness —
    /// points the shell at a specific build.
    #[test]
    #[serial_test::serial(process_env)]
    fn an_explicit_binary_override_is_searched_first() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = tmp.path().join("qos-catalogue");
        std::fs::write(&fake, b"#!/bin/true\n").unwrap();
        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::set_var("QOS_CATALOGUE_BIN", &fake) };
        let resolved = resolve_catalogue_bin();
        unsafe { std::env::remove_var("QOS_CATALOGUE_BIN") };
        assert_eq!(resolved.as_deref(), Some(fake.as_path()));
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn a_missing_override_falls_through_rather_than_resolving_to_it() {
        let tmp = tempfile::tempdir().unwrap();
        let absent = tmp.path().join("not-here");
        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::set_var("QOS_CATALOGUE_BIN", &absent) };
        let resolved = resolve_catalogue_bin();
        unsafe { std::env::remove_var("QOS_CATALOGUE_BIN") };
        assert_ne!(
            resolved.as_deref(),
            Some(absent.as_path()),
            "a path that does not exist must not be handed to Command::new"
        );
    }
}
