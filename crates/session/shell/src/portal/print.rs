//! `org.freedesktop.impl.portal.Print` backend.
//!
//! Spec: `/usr/share/dbus-1/interfaces/org.freedesktop.impl.portal.Print.xml`.
//! `PreparePrint` is the dialog half — a token in its `results` ties a
//! later `Print` to what the user configured; `Print` is the submission
//! half, with `fd` naming the document.
//!
//! The naive implementation hands an untrusted app a bidirectional socket
//! to CUPS — an IPP endpoint that enumerates destinations and can create
//! or cancel *other users'* jobs — trading the whole sandbox for one
//! feature. So the app gets only a file descriptor for bytes and a
//! prompt asking where to send them; everything else is decided here.
//! For the same reason [`Capability::Print`] never becomes a bind mount:
//! `tilo-launcher` turns a workspace's grants into sandbox binds in
//! `launcher::capabilities`, which binds only filesystem paths, devices and
//! network access, so no grant puts the CUPS socket inside the sandbox.
//!
//! **Both** methods go through [`flow::resolve`], including
//! `PreparePrint`: the printer list is itself the disclosure an app
//! could otherwise harvest without ever calling `Print`. See
//! `PrintBackend::prepare_print`.

use super::reply::{Ended, Reply, Results, answer};
#[cfg(target_os = "linux")]
use crate::shell::state::Capability;
#[cfg(target_os = "linux")]
use crate::shell::state::GRANT_ONCE;
#[cfg(target_os = "linux")]
use zbus::object_server::DispatchResult2;
#[cfg(target_os = "linux")]
use zbus::zvariant::OwnedObjectPath;
#[cfg(target_os = "linux")]
use zbus::{Connection, message::Header, message::Message};

use std::collections::HashMap;
#[cfg(any(test, target_os = "linux"))]
use std::io::{Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(any(test, target_os = "linux"))]
use zbus::zvariant::{OwnedValue, Value};

use super::PortalCaller;
#[cfg(target_os = "linux")]
use super::{
    CapabilityKind, PortalContext, auth::authenticate_call, flow, interface_dispatch, lockdown,
};

/// Hard ceiling on the bytes we will read off the app's descriptor.
///
/// 64 MiB clears any plausible desktop print job (a 500-page text PDF is
/// under 10 MB, a 100-page photo-heavy one ~50 MB) while bounding what
/// one D-Bus call can make the compositor allocate. The descriptor is
/// attacker-controlled and its `st_size` is not a promise, so the limit
/// is enforced on the read itself, not only the stat.
///
/// Exceeding it is an **error**, never a truncation: a print job
/// silently cut to 64 MiB is a corrupted document that reaches paper.
pub const MAX_SPOOL_BYTES: usize = 64 * 1024 * 1024;

/// Job-title ceiling in bytes: RFC 8011 types IPP's `job-name` as `name(MAX)` (255 octets); truncated here on a character boundary.
pub const MAX_TITLE_BYTES: usize = 255;

/// Copies ceiling — an app asking for `u32::MAX` copies is a
/// physical-resource `DoS` (paper, toner) no consent granularity fixes.
pub const MAX_COPIES: u32 = 100;

/// Title used when the app's own title sanitises down to nothing, in the
/// desktop's language, since it is what the print queue shows.
pub fn fallback_title() -> String {
    crate::shell::i18n::t("print-untitled-document")
}

/// How long a `PreparePrint` token stays redeemable — a dialog the user configured and walked away from shouldn't leave a live capability behind.
const TOKEN_TTL: Duration = Duration::from_secs(300);

/// Outstanding tokens held at once — bounds memory an app can pin by looping `PreparePrint` without ever printing; oldest is evicted past this.
const MAX_OUTSTANDING_TOKENS: usize = 32;

// Spooler abstraction

/// One job, fully decided, on its way to CUPS: `title` sanitised,
/// `destination` from the spooler's own list (never the app's request verbatim), `copies` clamped, `document` the bytes actually read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrintJobRequest {
    /// Which sandboxed app this is on behalf of.
    pub caller: PortalCaller,
    /// Sanitised job title.
    pub title: String,
    /// CUPS destination name.
    pub destination: String,
    /// Copy count, `1..=MAX_COPIES`.
    pub copies: u32,
    /// The document bytes.
    pub document: Vec<u8>,
}

/// Side-effect interface for talking to the print system; production shells out to `lp` ([`LpSpooler`]), tests inject a [`RecordingSpooler`].
pub trait PrintSpooler: Send + Sync {
    /// Destinations this machine can print to, **system default first** —
    /// this is the disclosure `PreparePrint` gates, since names alone routinely leak an employer, a building and a floor.
    fn destinations(&self) -> Vec<String>;

    /// Submit `job`. `Ok` carries the spooler's job id for the log.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `job.destination` is invalid, `job.document` is
    /// empty, the print system is unavailable, or the underlying spool
    /// command reports failure.
    fn submit(&self, job: PrintJobRequest) -> Result<String, String>;
}

/// Cheap-clone handle around `Arc<dyn PrintSpooler>`, matching [`super::RemoteInputInjectorHandle`].
#[derive(Clone)]
pub struct PrintSpoolerHandle {
    inner: Arc<dyn PrintSpooler>,
}

impl PrintSpoolerHandle {
    #[must_use]
    pub fn new(inner: Arc<dyn PrintSpooler>) -> Self {
        Self { inner }
    }

    /// Use `lp` against the local CUPS.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub fn cups() -> Self {
        Self {
            inner: Arc::new(LpSpooler),
        }
    }

    /// A handle with no print system behind it — every submission fails and the destination list is empty, surfacing as "no printers".
    #[must_use]
    pub fn null() -> Self {
        Self {
            inner: Arc::new(NullPrintSpooler),
        }
    }

    pub fn destinations(&self) -> Vec<String> {
        self.inner.destinations()
    }

    /// [`Self::destinations`] from async code. Listing CUPS destinations runs
    /// `lpstat`, a subprocess with a deadline of seconds, so it waits on the
    /// blocking pool instead of holding a portal worker.
    pub async fn destinations_blocking(&self) -> Vec<String> {
        let spooler = self.clone();
        tokio::task::spawn_blocking(move || spooler.destinations())
            .await
            .unwrap_or_default()
    }

    /// Submit `job` to the wrapped spooler.
    ///
    /// # Errors
    ///
    /// Returns `Err` under the same conditions as the wrapped
    /// [`PrintSpooler::submit`] implementation.
    pub fn submit(&self, job: PrintJobRequest) -> Result<String, String> {
        self.inner.submit(job)
    }
}

/// Production spooler: reads destinations with `lpstat`, submits with `lp`, always as **argv** — no shell in this path to interpolate into.
#[cfg(target_os = "linux")]
pub struct LpSpooler;

#[cfg(target_os = "linux")]
impl PrintSpooler for LpSpooler {
    fn destinations(&self) -> Vec<String> {
        // `lpstat -a` prints one line per destination, name first.
        let Ok(out) =
            bounded_command::run("lpstat", &["-a"], None, 1024 * 1024, Duration::from_secs(5))
        else {
            // No CUPS on this machine — an empty list is the truthful answer; `PreparePrint` turns it into an error rather than inventing a printer.
            return Vec::new();
        };
        if !out.status.success() {
            return Vec::new();
        }
        let mut names: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.split_whitespace().next())
            .filter(|n| valid_destination(n))
            .map(std::string::ToString::to_string)
            .collect();
        names.dedup();

        // Hoist the system default so `destinations()[0]` is the one an unprepared job should land on.
        if let Some(default) = lpstat_default()
            && let Some(pos) = names.iter().position(|n| *n == default)
        {
            names.swap(0, pos);
        }
        names
    }

    fn submit(&self, job: PrintJobRequest) -> Result<String, String> {
        use std::process::Command;

        // Re-checked even though `PreparePrint` already matched it — this is the last point before the string becomes argv.
        if !valid_destination(&job.destination) {
            return Err(format!("refusing unsafe destination {:?}", job.destination));
        }
        if job.document.is_empty() {
            return Err("refusing to spool an empty document".to_string());
        }

        // Argv, not a shell string: `-t <title>` consumes its value
        // unconditionally, so even a title shaped like a flag can't become one.
        // No document-format option is passed either — CUPS runs its own type detection over the actual bytes instead of trusting the app's claim.
        let mut command = Command::new("lp");
        command
            .arg("-d")
            .arg(&job.destination)
            .arg("-t")
            .arg(&job.title)
            .arg("-n")
            .arg(job.copies.to_string());
        let out = bounded_command::run_prepared_with_input_limit(
            command,
            Some(&job.document),
            MAX_SPOOL_BYTES,
            1024 * 1024,
            1024 * 1024,
            Duration::from_secs(5 * 60),
        )
        .map_err(|error| format!("print spooler failed: {error}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout)
                .chars()
                .filter(|character| !character.is_control())
                .take(512)
                .collect::<String>()
                .trim()
                .to_string())
        } else {
            Err(out.stderr_diagnostic())
        }
    }
}

/// `lpstat -d` → the system default destination, if there is one.
#[cfg(target_os = "linux")]
fn lpstat_default() -> Option<String> {
    let out =
        bounded_command::run("lpstat", &["-d"], None, 1024 * 1024, Duration::from_secs(5)).ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    // "system default destination: Office_LaserJet"
    let name = text.split(':').nth(1)?.trim().to_string();
    valid_destination(&name).then_some(name)
}

/// A spooler with nothing behind it.
pub struct NullPrintSpooler;

impl PrintSpooler for NullPrintSpooler {
    fn destinations(&self) -> Vec<String> {
        Vec::new()
    }
    fn submit(&self, _job: PrintJobRequest) -> Result<String, String> {
        Err("no print spooler configured".to_string())
    }
}

/// Records every submission instead of performing one — for tests that need to assert what actually reached the spooler.
pub struct RecordingSpooler {
    destinations: Vec<String>,
    jobs: Mutex<Vec<PrintJobRequest>>,
    fail: bool,
}

impl RecordingSpooler {
    /// A spooler advertising `destinations` (first entry is the default).
    pub fn with_destinations(destinations: Vec<String>) -> Self {
        Self {
            destinations,
            jobs: Mutex::new(Vec::new()),
            fail: false,
        }
    }

    /// A spooler that accepts jobs and then reports failure — for asserting the "spooler said no" response code.
    pub fn failing(destinations: Vec<String>) -> Self {
        Self {
            destinations,
            jobs: Mutex::new(Vec::new()),
            fail: true,
        }
    }

    /// Every job handed to [`PrintSpooler::submit`], in order.
    pub fn jobs(&self) -> Vec<PrintJobRequest> {
        self.jobs.lock().map(|j| j.clone()).unwrap_or_default()
    }
}

impl PrintSpooler for RecordingSpooler {
    fn destinations(&self) -> Vec<String> {
        self.destinations.clone()
    }

    fn submit(&self, job: PrintJobRequest) -> Result<String, String> {
        if let Ok(mut j) = self.jobs.lock() {
            j.push(job);
        }
        if self.fail {
            Err("recording spooler configured to fail".to_string())
        } else {
            Ok("recorded-1".to_string())
        }
    }
}

// Tokens

/// What a `PreparePrint` token redeems to. Note what is *not* here: the
/// app's settings/page-setup dicts, echoed back to the app and dropped rather than retained.
#[derive(Clone, Debug)]
pub struct PreparedJob {
    /// The app the token was minted for — worthless to anybody else, which is what makes it so.
    pub caller: PortalCaller,
    /// Destination the user's configuration resolved to.
    pub destination: String,
    pub copies: u32,
    pub issued: Instant,
}

/// Caller-bound preparation state, shared by the production portal and its policy tests.
#[derive(Default)]
pub struct PreparedPrintJobs {
    prepared: Arc<Mutex<HashMap<u32, PreparedJob>>>,
}

#[cfg(target_os = "linux")]
pub struct PrintBackend {
    ctx: PortalContext,
    jobs: PreparedPrintJobs,
}

#[cfg(target_os = "linux")]
impl PrintBackend {
    pub fn new(ctx: PortalContext) -> Self {
        Self {
            ctx,
            jobs: PreparedPrintJobs::default(),
        }
    }
    pub fn outstanding_tokens(&self) -> usize {
        self.jobs.outstanding_tokens()
    }
}

impl PreparedPrintJobs {
    /// Mint a token for `job` and return it, from `UnwrapErr(SysRng)` — never a counter,
    /// hash, or clock. The portal spec types it as a `u`, so 32 bits is the
    /// whole budget; [`Self::redeem_token`] also requires the redeemer to be
    /// the app it was minted for, and single use plus a TTL keep the guessing window small.
    ///
    /// Zero is never issued: the portal's `options` dict uses a missing key and zero interchangeably for "no token".
    pub fn mint_token(&self, job: PreparedJob) -> u32 {
        let Ok(mut table) = self.prepared.lock() else {
            return 0;
        };
        table.retain(|_, j| j.issued.elapsed() < TOKEN_TTL);
        while table.len() >= MAX_OUTSTANDING_TOKENS {
            let Some(oldest) = table
                .iter()
                .min_by_key(|(_, j)| j.issued)
                .map(|(token, _)| *token)
            else {
                break;
            };
            table.remove(&oldest);
        }
        let mut token = random_token();
        while token == 0 || table.contains_key(&token) {
            token = random_token();
        }
        table.insert(token, job);
        token
    }

    /// Redeem `token` on behalf of `caller`. Two properties, both pinned by tests:
    ///
    /// - **Single use**: a successful redemption removes the entry, so a replay finds nothing.
    /// - **Caller-bound, without collateral**: a token presented by the
    ///   wrong app is refused *and left in place* — removing it would let a
    ///   co-resident app destroy another's prepared job by guessing at it.
    pub fn redeem_token(&self, token: u32, caller: &PortalCaller) -> Option<PreparedJob> {
        let mut table = self.prepared.lock().ok()?;
        table.retain(|_, j| j.issued.elapsed() < TOKEN_TTL);
        let job = table.get(&token)?;
        if &job.caller != caller {
            tracing::warn!(
                app = %caller.app_id,
                "Print: refused a token minted for a different app"
            );
            return None;
        }
        table.remove(&token)
    }

    /// Outstanding token count — lets a test observe that a rejected redemption didn't consume somebody else's token.
    pub fn outstanding_tokens(&self) -> usize {
        self.prepared.lock().map_or(0, |t| t.len())
    }
}

fn random_token() -> u32 {
    u32::from_le_bytes(crate::random::bytes())
}

// D-Bus surface

/// The spec arguments of one `PreparePrint` call that policy reads. The
/// request handle and options are accepted on the wire and ignored.
#[cfg(target_os = "linux")]
struct PreparePrintRequest {
    app_id: String,
    parent_window: String,
    title: String,
    settings: HashMap<String, OwnedValue>,
    page_setup: HashMap<String, OwnedValue>,
}

/// `PreparePrint`'s wire arguments: `handle o`, `app_id s`, `parent_window s`,
/// `title s`, `settings a{sv}`, `page_setup a{sv}`, `options a{sv}`.
#[cfg(target_os = "linux")]
type PreparePrintArgs = (
    OwnedObjectPath,
    String,
    String,
    String,
    HashMap<String, OwnedValue>,
    HashMap<String, OwnedValue>,
    HashMap<String, OwnedValue>,
);

#[cfg(target_os = "linux")]
impl From<PreparePrintArgs> for PreparePrintRequest {
    fn from(
        (_handle, app_id, parent_window, title, settings, page_setup, _options): PreparePrintArgs,
    ) -> Self {
        Self {
            app_id,
            parent_window,
            title,
            settings,
            page_setup,
        }
    }
}

/// The spec arguments of one `Print` call that policy reads; the request
/// handle is accepted on the wire and ignored.
#[cfg(target_os = "linux")]
struct PrintRequest {
    app_id: String,
    parent_window: String,
    title: String,
    fd: zbus::zvariant::OwnedFd,
    options: HashMap<String, OwnedValue>,
}

/// `Print`'s wire arguments: `handle o`, `app_id s`, `parent_window s`,
/// `title s`, `fd h`, `options a{sv}`.
#[cfg(target_os = "linux")]
type PrintArgs = (
    OwnedObjectPath,
    String,
    String,
    String,
    zbus::zvariant::OwnedFd,
    HashMap<String, OwnedValue>,
);

#[cfg(target_os = "linux")]
impl From<PrintArgs> for PrintRequest {
    fn from((_handle, app_id, parent_window, title, fd, options): PrintArgs) -> Self {
        Self {
            app_id,
            parent_window,
            title,
            fd,
            options,
        }
    }
}

/// The interface's introspection, byte-identical to what zbus's
/// `#[interface]` macro wrote for the former macro-dispatched backend.
#[cfg(target_os = "linux")]
const PRINT_INTROSPECTION: &str = r#"<interface name="org.freedesktop.impl.portal.Print">
  <!--
   `PreparePrint` — the print dialog, minus the dialog.

   **This prompts**, even though nothing has been printed yet: the reply
   itself is sensitive — printer names read like `ACME-3F-Reception`,
   disclosing an employer, site and floor plan, and fingerprinting the
   network. An app that only ever calls `PreparePrint` would collect all
   of that without a prompt firing if submission were the only gate.
   Costs the honest app nothing: `flow::resolve` fast-paths a standing `Always` grant.
   -->
  <method name="PreparePrint">
    <arg name="_handle" type="o" direction="in"/>
    <arg name="app_id" type="s" direction="in"/>
    <arg name="parent_window" type="s" direction="in"/>
    <arg name="title" type="s" direction="in"/>
    <arg name="settings" type="a{sv}" direction="in"/>
    <arg name="page_setup" type="a{sv}" direction="in"/>
    <arg name="_options" type="a{sv}" direction="in"/>
    <arg type="u" direction="out"/>
    <arg type="a{sv}" direction="out"/>
  </method>
  <!--
   `Print` — hand over the bytes. `fd` is a descriptor the app chose;
   treat every property as a claim: read only after consent, only if a regular file, only up to [`MAX_SPOOL_BYTES`].
   -->
  <method name="Print">
    <arg name="_handle" type="o" direction="in"/>
    <arg name="app_id" type="s" direction="in"/>
    <arg name="parent_window" type="s" direction="in"/>
    <arg name="title" type="s" direction="in"/>
    <arg name="fd" type="h" direction="in"/>
    <arg name="options" type="a{sv}" direction="in"/>
    <arg type="u" direction="out"/>
    <arg type="a{sv}" direction="out"/>
  </method>
</interface>
"#;

#[cfg(target_os = "linux")]
interface_dispatch::method_interface!(
    PrintBackend,
    "org.freedesktop.impl.portal.Print",
    PRINT_INTROSPECTION
);

#[cfg(target_os = "linux")]
impl PrintBackend {
    fn dispatch_call<'call>(
        &'call self,
        connection: &'call Connection,
        message: &'call Message,
        name: &str,
    ) -> DispatchResult2<'call> {
        match name {
            "PreparePrint" => interface_dispatch::dispatch(
                connection,
                message,
                move |hdr, args: PreparePrintArgs| async move {
                    Ok(self.prepare_print(&hdr, args.into()).await)
                },
            ),
            "Print" => interface_dispatch::dispatch(
                connection,
                message,
                move |hdr, args: PrintArgs| async move { Ok(self.print(&hdr, args.into()).await) },
            ),
            _ => DispatchResult2::NotFound,
        }
    }

    /// `PreparePrint` — the print dialog, minus the dialog.
    ///
    /// **This prompts**, even though nothing has been printed yet: the reply
    /// itself is sensitive — printer names read like `ACME-3F-Reception`,
    /// disclosing an employer, site and floor plan, and fingerprinting the
    /// network. An app that only ever calls `PreparePrint` would collect all
    /// of that without a prompt firing if submission were the only gate.
    /// Costs the honest app nothing: `flow::resolve` fast-paths a standing `Always` grant.
    async fn prepare_print(&self, hdr: &Header<'_>, request: PreparePrintRequest) -> Reply {
        answer(async move {
            let PreparePrintRequest {
                app_id,
                parent_window,
                title,
                settings,
                page_setup,
            } = request;
            tracing::debug!("Print.PreparePrint: app={app_id} title={title:?}");
            let Ok(auth) = authenticate_call(&self.ctx, &hdr, &app_id, Some(&parent_window)).await
            else {
                tracing::warn!(app = %app_id, "Print.PreparePrint: unresolved portal caller");
                return Err(Ended::Failed);
            };
            if lockdown::current_policy().disable_printing {
                return Err(Ended::Cancelled);
            }
            let caller = auth.caller;

            let outcome = flow::resolve(
                &self.ctx,
                caller.clone(),
                CapabilityKind::Print,
                Capability::Print { scope: GRANT_ONCE },
            )
            .await;
            if !outcome.is_allowed() {
                return Err(Ended::Cancelled);
            }

            let available = self.ctx.print_spooler.destinations_blocking().await;
            let requested = crate::portal::option::<String>(&settings, "printer");
            let Some(destination) = choose_destination(&available, requested.as_deref()) else {
                // No printers, or the app named one this machine doesn't have — both "other error" rather than a silent substitution.
                tracing::debug!(
                    "Print.PreparePrint: no usable destination (requested {requested:?}, \
                 {} available)",
                    available.len()
                );
                return Err(Ended::Failed);
            };
            let copies = clamp_copies(opt_copies(&settings));

            let token = self.jobs.mint_token(PreparedJob {
                caller,
                destination: destination.clone(),
                copies,
                issued: Instant::now(),
            });
            if token == 0 {
                return Err(Ended::Failed);
            }

            Ok(prepare_results(&destination, copies, token, &page_setup))
        })
        .await
    }

    /// `Print` — hand over the bytes. `fd` is a descriptor the app chose;
    /// treat every property as a claim: read only after consent, only if a regular file, only up to [`MAX_SPOOL_BYTES`].
    async fn print(&self, hdr: &Header<'_>, request: PrintRequest) -> Reply {
        answer(async move {
            let PrintRequest {
                app_id,
                parent_window,
                title,
                fd,
                options,
            } = request;
            tracing::debug!("Print.Print: app={app_id} title={title:?}");
            let Ok(auth) = authenticate_call(&self.ctx, &hdr, &app_id, Some(&parent_window)).await
            else {
                tracing::warn!(app = %app_id, "Print.Print: unresolved portal caller");
                return Err(Ended::Failed);
            };
            if lockdown::current_policy().disable_printing {
                return Err(Ended::Cancelled);
            }
            let caller = auth.caller;

            // Consent first — before we read a byte, so a denial costs the compositor nothing and touches nothing.
            let outcome = flow::resolve(
                &self.ctx,
                caller.clone(),
                CapabilityKind::Print,
                Capability::Print { scope: GRANT_ONCE },
            )
            .await;
            if !outcome.is_allowed() {
                return Err(Ended::Cancelled);
            }

            // A token, if present, must redeem — unknown/expired/replayed/foreign
            // is refused outright, not downgraded to "no token", since falling back to the default destination would let a replayed token still print.
            let prepared = match crate::portal::option::<u32>(&options, "token") {
                None | Some(0) => None,
                Some(token) => {
                    if let Some(job) = self.jobs.redeem_token(token, &caller) {
                        Some(job)
                    } else {
                        tracing::warn!(
                            app = %caller.app_id,
                            "Print: refused an unknown, replayed or expired token"
                        );
                        return Err(Ended::Failed);
                    }
                }
            };

            let destination = if let Some(job) = prepared.as_ref() {
                job.destination.clone()
            } else {
                let available = self.ctx.print_spooler.destinations_blocking().await;
                if let Some(d) = available.first() {
                    d.clone()
                } else {
                    tracing::debug!("Print.Print: no destinations available");
                    return Err(Ended::Failed);
                }
            };
            let copies = prepared.as_ref().map_or(1, |j| j.copies);
            let job_title = sanitise_title(&title);

            let spooler = self.ctx.print_spooler.clone();
            let submitted = tokio::task::spawn_blocking(move || -> Result<String, String> {
                let document = spool_from_fd(fd)?;
                spooler.submit(PrintJobRequest {
                    caller,
                    title: job_title,
                    destination,
                    copies,
                    document,
                })
            })
            .await;

            match submitted {
                Ok(Ok(job_id)) => {
                    tracing::info!("Print.Print: submitted job {job_id}");
                    Ok(Results::new())
                }
                Ok(Err(e)) => {
                    tracing::warn!("Print.Print: submission refused: {e}");
                    Err(Ended::Failed)
                }
                Err(e) => {
                    tracing::warn!("Print.Print: spool task failed: {e}");
                    Err(Ended::Failed)
                }
            }
        })
        .await
    }
}

// Pure helpers — every one on the production path above.

/// Read the document off the app's descriptor, defensively: the
/// descriptor is attacker-controlled, so regular files only (a FIFO or
/// socket with no writer blocks forever; a character device can be
/// infinite), seek to the start (the app may have left the offset at
/// EOF), and read one byte past the cap so an oversize document is *detected*, not silently truncated.
///
/// # Errors
///
/// Returns an error if the descriptor is not a regular file, exceeds the size limit, or cannot be read.
#[cfg(any(test, target_os = "linux"))]
pub(super) fn spool_from_fd(fd: zbus::zvariant::OwnedFd) -> Result<Vec<u8>, String> {
    let std_fd: std::os::fd::OwnedFd = fd.into();
    let mut file = std::fs::File::from(std_fd);

    let meta = file
        .metadata()
        .map_err(|e| format!("cannot stat document fd: {e}"))?;
    if !meta.is_file() {
        return Err("document fd is not a regular file".to_string());
    }
    // Cheap pre-check, not a substitute for the bounded read below — `st_size` is a claim too, and the file can grow after the stat.
    if meta.len() > MAX_SPOOL_BYTES as u64 {
        return Err(format!(
            "document is {} bytes, over the {MAX_SPOOL_BYTES}-byte print limit",
            meta.len()
        ));
    }

    let _ = file.seek(SeekFrom::Start(0));

    let mut buf = Vec::new();
    file.take((MAX_SPOOL_BYTES as u64).saturating_add(1))
        .read_to_end(&mut buf)
        .map_err(|e| format!("reading document: {e}"))?;
    if buf.len() > MAX_SPOOL_BYTES {
        return Err(format!(
            "document exceeds the {MAX_SPOOL_BYTES}-byte print limit"
        ));
    }
    if buf.is_empty() {
        return Err("document is empty".to_string());
    }
    Ok(buf)
}

/// Make an app-supplied job title safe to put in an argv, a CUPS queue
/// listing and a log line — the one attacker-controlled string that
/// reaches a process argument, so it gets the full treatment:
///
/// - Control characters become spaces (not dropped, so `"a\nb"` can't
///   collapse into `"ab"`): a newline forges a log line, ESC drives
///   whatever terminal renders the queue, NUL truncates a C string.
/// - Bidi overrides go too — `U+202E` reverses rendering, which is how
///   `fdp.exe` shows up in a queue as `exe.pdf`.
/// - Leading dashes are stripped, so the value can never read as a flag.
/// - Truncated to [`MAX_TITLE_BYTES`] on a character boundary.
/// - Empty after all that falls back to [`fallback_title`].
pub fn sanitise_title(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        let safe = if is_unsafe_title_char(ch) { ' ' } else { ch };
        if out.len().saturating_add(safe.len_utf8()) > MAX_TITLE_BYTES {
            break;
        }
        out.push(safe);
    }
    let trimmed = out.trim().trim_start_matches('-').trim();
    if trimmed.is_empty() {
        fallback_title()
    } else {
        trimmed.to_string()
    }
}

/// Control characters plus the Unicode directional-formatting set.
fn is_unsafe_title_char(ch: char) -> bool {
    ch.is_control()
        || matches!(
            ch,
            '\u{200E}'
                | '\u{200F}'
                | '\u{202A}'
                | '\u{202B}'
                | '\u{202C}'
                | '\u{202D}'
                | '\u{202E}'
                | '\u{2066}'
                | '\u{2067}'
                | '\u{2068}'
                | '\u{2069}'
        )
}

/// A CUPS destination name we're willing to put in an argv. CUPS itself
/// rejects space, `/` and `#` and caps at 127 characters; we go narrower
/// — only `[A-Za-z0-9._-]`, non-empty, not leading with `-` — so the value can never be read as an option.
pub fn valid_destination(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 127
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Resolve the destination for a prepared job: an app may *ask* for a
/// printer, but membership in `available` is what keeps an app-authored string out of the argv; an absent name returns `None`, never a substitute.
pub fn choose_destination(available: &[String], requested: Option<&str>) -> Option<String> {
    match requested {
        Some(want) if !want.is_empty() => available
            .iter()
            .find(|d| d.as_str() == want && valid_destination(d))
            .cloned(),
        _ => available.iter().find(|d| valid_destination(d)).cloned(),
    }
}

/// Clamp a requested copy count into `1..=MAX_COPIES`.
pub fn clamp_copies(requested: Option<u32>) -> u32 {
    requested.unwrap_or(1).clamp(1, MAX_COPIES)
}

/// `PreparePrint` results: the settings and page setup the user's side agreed to, plus the token that ties a later `Print` to them.
#[cfg(any(test, target_os = "linux"))]
fn prepare_results(
    destination: &str,
    copies: u32,
    token: u32,
    page_setup: &HashMap<String, OwnedValue>,
) -> HashMap<String, OwnedValue> {
    let mut results: HashMap<String, OwnedValue> = HashMap::new();

    // `settings` is normalised, not echoed — it reports what was actually resolved, not the app's original ask.
    let mut settings: HashMap<String, Value<'static>> = HashMap::new();
    settings.insert("printer".to_string(), Value::new(destination.to_string()));
    settings.insert("n-copies".to_string(), Value::new(copies.to_string()));
    if let Ok(v) = OwnedValue::try_from(Value::new(settings)) {
        results.insert("settings".to_string(), v);
    }

    // `page-setup` goes back untouched — the app's own geometry returning to the app that sent it; we have no opinion on it and don't retain it.
    let echoed: HashMap<String, Value<'static>> = page_setup
        .iter()
        .filter_map(|(k, v)| v.try_clone().ok().map(|c| (k.clone(), Value::from(c))))
        .collect();
    if let Ok(v) = OwnedValue::try_from(Value::new(echoed)) {
        results.insert("page-setup".to_string(), v);
    }

    if let Ok(v) = OwnedValue::try_from(Value::U32(token)) {
        results.insert("token".to_string(), v);
    }
    results
}

/// Copy count out of a `GtkPrintSettings`-shaped dict; GTK stores every setting as a string (`n-copies` arrives as `"3"`), but a real `u` is accepted too.
#[cfg(any(test, target_os = "linux"))]
fn opt_copies(settings: &HashMap<String, OwnedValue>) -> Option<u32> {
    if let Some(n) = crate::portal::option::<u32>(settings, "n-copies") {
        return Some(n);
    }
    crate::portal::option::<String>(settings, "n-copies")?
        .trim()
        .parse::<u32>()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use crate::portal::{CallerMap, GrantStoreHandle, PromptQueueHandle, PromptResolver};
    #[cfg(target_os = "linux")]
    use crate::shell::state::Workspace;
    use crate::shell::state::WorkspaceId;
    use std::io::Write;

    #[cfg(target_os = "linux")]
    #[test]
    fn print_methods_decode_exactly_the_advertised_arguments() {
        use zbus::zvariant::Type;
        let advertised = |method| {
            format!(
                "({})",
                interface_dispatch::advertised_in_signature(PRINT_INTROSPECTION, method)
            )
        };
        assert_eq!(
            advertised("PreparePrint"),
            <PreparePrintArgs as Type>::SIGNATURE.to_string()
        );
        assert_eq!(
            advertised("Print"),
            <PrintArgs as Type>::SIGNATURE.to_string()
        );
    }

    #[cfg(target_os = "linux")]
    fn ctx_with(
        spooler: PrintSpoolerHandle,
        resolver: PromptResolver,
        app: &str,
    ) -> (PortalContext, WorkspaceId) {
        let (store, ws_id) =
            GrantStoreHandle::test_with_workspace(Workspace::new(WorkspaceId(90), "pr"));
        let map = CallerMap::new();
        map.force_caller_for_tests(PortalCaller::new(ws_id, app));
        let mut ctx =
            PortalContext::new_with_null_spawners(store, PromptQueueHandle::new(resolver), map);
        ctx.print_spooler = spooler;
        (ctx, ws_id)
    }

    #[cfg(target_os = "linux")]
    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    // Title sanitisation

    #[test]
    fn sanitise_title_produces_a_safe_bounded_ipp_name() {
        // Phase `sanitise_title_replaces_control_characters_with_spaces`.
        // Replaced, not dropped: the two halves must stay separated so a forged second line can't fuse onto the first.
        assert_eq!(sanitise_title("quarterly\nreport"), "quarterly report");
        assert_eq!(sanitise_title("a\u{1b}[31mb"), "a [31mb");
        assert_eq!(sanitise_title("nul\0byte"), "nul byte");
        // Phase `sanitise_title_strips_bidi_overrides`.
        {
            let spoofed = format!("invoice{}fdp.exe", '\u{202E}');
            let cleaned = sanitise_title(&spoofed);
            assert!(
                !cleaned.contains('\u{202E}'),
                "a right-to-left override must not survive into the queue listing: {cleaned:?}"
            );
        }
        // Phase `sanitise_title_strips_leading_dashes`.
        assert_eq!(sanitise_title("--option-looking"), "option-looking");
        assert_eq!(sanitise_title("  -n 9999"), "n 9999");
        // Phase `sanitise_title_truncates_to_the_ipp_name_limit`.
        {
            let long = "x".repeat(4096);
            let out = sanitise_title(&long);
            assert_eq!(out.len(), MAX_TITLE_BYTES);
        }
        // Phase `sanitise_title_truncates_multibyte_on_a_char_boundary`.
        {
            // 'é' is two bytes; 255 is odd, so a naive byte slice would split the last one and panic.
            let long = "é".repeat(4096);
            let out = sanitise_title(&long);
            assert!(out.len() <= MAX_TITLE_BYTES);
            assert!(out.chars().all(|c| c == 'é'));
        }
        // Phase `sanitise_title_falls_back_when_nothing_survives`.
        assert_eq!(sanitise_title(""), fallback_title());
        assert_eq!(sanitise_title("\n\t  \r"), fallback_title());
        assert_eq!(sanitise_title("---"), fallback_title());
    }

    // Destination validation

    #[test]
    fn print_destinations_are_real_safe_printers() {
        // Phase `valid_destination_rejects_argv_and_path_shapes`.
        assert!(valid_destination("Office_LaserJet"));
        assert!(valid_destination("front-desk.2"));
        assert!(!valid_destination(""));
        assert!(!valid_destination("-o"));
        assert!(!valid_destination("has space"));
        assert!(!valid_destination("../../etc/passwd"));
        assert!(!valid_destination("a;rm -rf /"));
        assert!(!valid_destination("$(id)"));
        assert!(!valid_destination(&"a".repeat(128)));
        // Phase `choose_destination_only_returns_printers_the_machine_has`.
        {
            let available = vec!["Office".to_string(), "Lab".to_string()];
            assert_eq!(
                choose_destination(&available, Some("Lab")),
                Some("Lab".to_string())
            );
            // Default = first entry (the spooler puts the system default there).
            assert_eq!(
                choose_destination(&available, None),
                Some("Office".to_string())
            );
            // A printer this machine does not have is refused, not substituted.
            assert_eq!(choose_destination(&available, Some("Elsewhere")), None);
            // ...including one whose *name* is an injection attempt.
            assert_eq!(choose_destination(&available, Some("-o raw")), None);
            assert_eq!(choose_destination(&[], None), None);
        }
        // Phase `null_spooler_reports_no_printers_rather_than_inventing_one`.
        {
            let h = PrintSpoolerHandle::null();
            assert!(h.destinations().is_empty());
            assert!(
                h.submit(PrintJobRequest {
                    caller: PortalCaller::new(WorkspaceId(1), "app"),
                    title: "t".into(),
                    destination: "Office".into(),
                    copies: 1,
                    document: b"x".to_vec(),
                })
                .is_err()
            );
        }
        // Phase `recording_spooler_captures_what_was_submitted`.
        {
            let rec = RecordingSpooler::with_destinations(vec!["Office".to_string()]);
            assert!(rec.jobs().is_empty());
            let job = PrintJobRequest {
                caller: PortalCaller::new(WorkspaceId(5), "app"),
                title: "Report".into(),
                destination: "Office".into(),
                copies: 2,
                document: b"%PDF-1.7".to_vec(),
            };
            assert!(rec.submit(job.clone()).is_ok());
            assert_eq!(rec.jobs(), vec![job]);
        }
    }

    #[test]
    fn prepare_results_carry_bounded_copies_and_settings() {
        // Phase `clamp_copies_bounds_the_request`.
        assert_eq!(clamp_copies(None), 1);
        assert_eq!(clamp_copies(Some(0)), 1);
        assert_eq!(clamp_copies(Some(3)), 3);
        assert_eq!(clamp_copies(Some(u32::MAX)), MAX_COPIES);
        // Phase `opt_copies_accepts_gtk_string_and_native_u32`.
        {
            let mut s: HashMap<String, OwnedValue> = HashMap::new();
            s.insert(
                "n-copies".to_string(),
                OwnedValue::try_from(Value::new("7".to_string())).unwrap(),
            );
            assert_eq!(opt_copies(&s), Some(7));

            let mut n: HashMap<String, OwnedValue> = HashMap::new();
            n.insert(
                "n-copies".to_string(),
                OwnedValue::try_from(Value::U32(4)).unwrap(),
            );
            assert_eq!(opt_copies(&n), Some(4));

            assert_eq!(opt_copies(&HashMap::new()), None);
        }
        // Phase `prepare_results_carry_settings_page_setup_and_token`.
        {
            let mut page_setup: HashMap<String, OwnedValue> = HashMap::new();
            page_setup.insert(
                "PPDName".to_string(),
                OwnedValue::try_from(Value::new("A4".to_string())).unwrap(),
            );
            let results = prepare_results("Office", 3, 0xDEAD_BEEF, &page_setup);

            // The three keys the frontend reads back.
            let token = u32::try_from(results.get("token").unwrap().try_clone().unwrap()).unwrap();
            assert_eq!(token, 0xDEAD_BEEF);

            let settings: HashMap<String, OwnedValue> =
                HashMap::try_from(results.get("settings").unwrap().try_clone().unwrap()).unwrap();
            assert_eq!(
                String::try_from(settings.get("printer").unwrap().try_clone().unwrap()).unwrap(),
                "Office"
            );
            assert_eq!(
                String::try_from(settings.get("n-copies").unwrap().try_clone().unwrap()).unwrap(),
                "3"
            );

            let echoed: HashMap<String, OwnedValue> =
                HashMap::try_from(results.get("page-setup").unwrap().try_clone().unwrap()).unwrap();
            assert_eq!(
                String::try_from(echoed.get("PPDName").unwrap().try_clone().unwrap()).unwrap(),
                "A4"
            );
        }
    }

    // Fd handling

    #[test]
    fn spool_from_fd_reads_only_bounded_regular_documents() {
        // Phase `spool_from_fd_reads_a_regular_file_from_the_start`.
        {
            let mut f = tempfile::NamedTempFile::new().unwrap();
            f.write_all(b"%PDF-1.7 hello").unwrap();
            f.flush().unwrap();
            // Re-open and seek to EOF so the read has to rewind for itself.
            let mut file = std::fs::File::open(f.path()).unwrap();
            file.seek(SeekFrom::End(0)).unwrap();
            let owned: std::os::fd::OwnedFd = file.into();
            let bytes = spool_from_fd(zbus::zvariant::OwnedFd::from(owned)).expect("spool");
            assert_eq!(bytes, b"%PDF-1.7 hello");
        }
        // Phase `spool_from_fd_rejects_a_non_regular_descriptor`.
        {
            // A socket (or FIFO) with no writer blocks forever; requiring a regular file removes that as a hang vector.
            let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
            let owned: std::os::fd::OwnedFd = a.into();
            let err = spool_from_fd(zbus::zvariant::OwnedFd::from(owned))
                .expect_err("a socket must not be spooled");
            assert!(err.contains("regular file"), "unexpected error: {err}");
        }
        // Phase `spool_from_fd_refuses_an_oversize_document`.
        {
            let f = tempfile::NamedTempFile::new().unwrap();
            // Sparse: one byte past the cap, costing no disk.
            f.as_file().set_len(MAX_SPOOL_BYTES as u64 + 1).unwrap();
            let file = std::fs::File::open(f.path()).unwrap();
            let owned: std::os::fd::OwnedFd = file.into();
            let err = spool_from_fd(zbus::zvariant::OwnedFd::from(owned))
                .expect_err("over the cap must fail");
            assert!(err.contains("limit"), "unexpected error: {err}");
        }
        // Phase `spool_from_fd_refuses_an_empty_document`.
        {
            let f = tempfile::NamedTempFile::new().unwrap();
            let file = std::fs::File::open(f.path()).unwrap();
            let owned: std::os::fd::OwnedFd = file.into();
            assert!(spool_from_fd(zbus::zvariant::OwnedFd::from(owned)).is_err());
        }
    }

    // Tokens

    fn prepared_for(caller: &PortalCaller) -> PreparedJob {
        PreparedJob {
            caller: caller.clone(),
            destination: "Office".to_string(),
            copies: 1,
            issued: Instant::now(),
        }
    }

    #[test]
    fn print_tokens_are_unique_bounded_and_redeemed_once() {
        // Phase `tokens_are_non_zero_and_do_not_repeat`.
        {
            let ws = WorkspaceId(90);
            let backend = PreparedPrintJobs::default();
            let caller = PortalCaller::new(ws, "org.example.print");
            let mut seen = std::collections::HashSet::new();
            // MAX_OUTSTANDING_TOKENS mints fit without eviction, so every value has to be distinct *and* live at the same time.
            for _ in 0..MAX_OUTSTANDING_TOKENS {
                let t = backend.mint_token(prepared_for(&caller));
                assert_ne!(t, 0, "zero is the 'no token' sentinel and must never issue");
                assert!(seen.insert(t), "minted a duplicate token");
            }
            assert_eq!(backend.outstanding_tokens(), MAX_OUTSTANDING_TOKENS);
        }
        // Phase `token_table_is_bounded`.
        {
            let ws = WorkspaceId(90);
            let backend = PreparedPrintJobs::default();
            let caller = PortalCaller::new(ws, "org.example.print");
            for _ in 0..(MAX_OUTSTANDING_TOKENS * 4) {
                backend.mint_token(prepared_for(&caller));
            }
            assert_eq!(
                backend.outstanding_tokens(),
                MAX_OUTSTANDING_TOKENS,
                "an app must not be able to pin unbounded token state"
            );
        }
        // Phase `a_foreign_token_is_refused_without_consuming_it`.
        {
            let ws = WorkspaceId(90);
            let backend = PreparedPrintJobs::default();
            let app_a = PortalCaller::new(ws, "org.example.a");
            let app_b = PortalCaller::new(ws, "org.example.b");
            let token = backend.mint_token(prepared_for(&app_a));

            assert!(
                backend.redeem_token(token, &app_b).is_none(),
                "app B must not redeem app A's token"
            );
            assert_eq!(
                backend.outstanding_tokens(),
                1,
                "a refused redemption must not destroy the real owner's token"
            );
            assert!(
                backend.redeem_token(token, &app_a).is_some(),
                "app A's own token must still work after B's attempt"
            );
        }
        // Phase `a_token_redeems_exactly_once`.
        {
            let ws = WorkspaceId(90);
            let backend = PreparedPrintJobs::default();
            let caller = PortalCaller::new(ws, "org.example.a");
            let token = backend.mint_token(prepared_for(&caller));
            assert!(backend.redeem_token(token, &caller).is_some());
            assert!(
                backend.redeem_token(token, &caller).is_none(),
                "a replayed token must not redeem a second time"
            );
        }
    }

    // Results shape

    #[test]
    #[cfg(target_os = "linux")]
    fn lp_spooler_is_replaceable_and_refuses_unsafe_destinations() {
        // Phase `lp_spooler_refuses_an_unsafe_destination_before_spawning`.
        {
            // Reaching `lp` at all with this would be the bug; the guard is the last check before the string becomes argv.
            let err = LpSpooler
                .submit(PrintJobRequest {
                    caller: PortalCaller::new(WorkspaceId(1), "app"),
                    title: "t".into(),
                    destination: "-o raw".into(),
                    copies: 1,
                    document: b"x".to_vec(),
                })
                .expect_err("unsafe destination must not reach lp");
            assert!(err.contains("unsafe destination"), "unexpected: {err}");
        }
        // Phase `ctx_default_spooler_is_replaceable`.
        {
            // The backend reads the spooler off the context, so a test that
            // injects one must actually see it — guards against a future refactor hard-wiring `LpSpooler` inside the backend.
            let rec = Arc::new(RecordingSpooler::with_destinations(vec![
                "Injected".to_string(),
            ]));
            let (ctx, _ws) = ctx_with(
                PrintSpoolerHandle::new(rec),
                PromptResolver::AutoAllow,
                "org.example.print",
            );
            assert_eq!(ctx.print_spooler.destinations(), vec!["Injected"]);
            let _ = rt();
        }
    }
}
