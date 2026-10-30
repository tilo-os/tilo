//! `org.freedesktop.impl.portal.Print`.
//!
//! `PreparePrint` is the dialog: the person picks a printer in a question
//! of kind [`PRINT_QUESTION`] and the app gets a token tying a later
//! `Print` to that choice. `Print` hands over the document as a file
//! descriptor; without a token it asks the same question first.
//!
//! The app never reaches CUPS itself (its IPP socket lists every printer
//! and can cancel other people's jobs): it gets only a descriptor for
//! bytes, and the printer names, which can disclose an employer, a
//! building and a floor, reach it only after the person chose one. Jobs go
//! to `lp` as argv: no shell, a destination from the spooler's own list,
//! a title cleaned of controls and bidi overrides, a bounded copy count.

use std::collections::HashMap;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;
use zbus::Connection;
use zbus::message::{Header, Message};
use zbus::object_server::DispatchResult2;
use zbus::zvariant::{OwnedFd, OwnedObjectPath, OwnedValue, Value};

use crate::reply::{Ended, Reply, Results, answer};
use crate::{Portal, PortalCaller, dispatch, lockdown, option};

/// The kind of the question choosing a printer asks.
pub const PRINT_QUESTION: &str = "portal.print";

/// The largest document spooled.
pub const MAX_SPOOL_BYTES: usize = 64 * 1024 * 1024;
/// IPP's `job-name` is at most 255 octets.
pub const MAX_TITLE_BYTES: usize = 255;
/// An app asking for millions of copies wastes paper no consent covers.
pub const MAX_COPIES: u32 = 100;
/// How long a prepared job may be printed.
const TOKEN_TTL: Duration = Duration::from_secs(300);
/// The prepared jobs one session keeps at once; the oldest goes first.
const MAX_OUTSTANDING_TOKENS: usize = 32;

/// What the sheet shows for choosing a printer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrintQuestion {
    /// The document's title, cleaned.
    pub title: String,
    /// The printers, the system default first: the answer's choice counts
    /// into it.
    pub printers: Vec<String>,
    /// The printer preselected: the one the app asked for, else the
    /// default.
    pub selected: usize,
    /// How many copies the app asked for.
    pub copies: u32,
}

/// One job on its way to the printer, fully decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrintJob {
    pub caller: PortalCaller,
    pub title: String,
    pub destination: String,
    pub copies: u32,
    pub document: Vec<u8>,
}

/// The print system.
pub trait Spooler: Send + Sync {
    /// The printers, the system default first.
    fn destinations(&self) -> Vec<String>;

    /// Print `job`; the spooler's job id.
    ///
    /// # Errors
    /// The spooler refused it.
    fn submit(&self, job: PrintJob) -> Result<String, String>;
}

/// CUPS, through `lpstat` and `lp`.
pub struct Cups;

impl Spooler for Cups {
    fn destinations(&self) -> Vec<String> {
        let Ok(out) =
            bounded_command::run("lpstat", &["-a"], None, 1024 * 1024, Duration::from_secs(5))
        else {
            return Vec::new();
        };
        if !out.status.success() {
            return Vec::new();
        }
        let mut names: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .filter(|name| valid_destination(name))
            .map(str::to_owned)
            .collect();
        names.dedup();
        if let Some(default) = lpstat_default()
            && let Some(position) = names.iter().position(|name| *name == default)
        {
            names.swap(0, position);
        }
        names
    }

    fn submit(&self, job: PrintJob) -> Result<String, String> {
        if !valid_destination(&job.destination) {
            return Err(format!("refusing destination {:?}", job.destination));
        }
        if job.document.is_empty() {
            return Err("refusing an empty document".into());
        }
        // `-t` takes its value whatever it looks like, and no document
        // format is passed: CUPS detects the type from the bytes.
        let mut command = std::process::Command::new("lp");
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
        .map_err(|error| format!("lp failed: {error}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout)
                .chars()
                .filter(|c| !c.is_control())
                .take(512)
                .collect::<String>()
                .trim()
                .to_owned())
        } else {
            Err(out.stderr_diagnostic())
        }
    }
}

/// `lpstat -d`: the system default printer.
fn lpstat_default() -> Option<String> {
    let out =
        bounded_command::run("lpstat", &["-d"], None, 1024 * 1024, Duration::from_secs(5)).ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let name = text.split(':').nth(1)?.trim().to_owned();
    valid_destination(&name).then_some(name)
}

/// A job the person prepared, redeemable once by the app it was for.
#[derive(Clone, Debug)]
struct Prepared {
    caller: PortalCaller,
    destination: String,
    copies: u32,
    issued: Instant,
}

/// The prepared jobs, by token.
#[derive(Clone, Default)]
struct Tokens {
    prepared: Arc<Mutex<HashMap<u32, Prepared>>>,
}

impl Tokens {
    /// A random, never zero token for `job` (zero means no token).
    fn mint(&self, job: Prepared) -> u32 {
        let mut table = self.prepared.lock_recover();
        table.retain(|_, job| job.issued.elapsed() < TOKEN_TTL);
        while table.len() >= MAX_OUTSTANDING_TOKENS {
            let Some(oldest) = table
                .iter()
                .min_by_key(|(_, job)| job.issued)
                .map(|(token, _)| *token)
            else {
                break;
            };
            table.remove(&oldest);
        }
        let mut token = 0;
        while token == 0 || table.contains_key(&token) {
            token = rand::random();
        }
        table.insert(token, job);
        token
    }

    /// Redeem `token` for `caller`, once. A token another app presents is
    /// refused and left in place, so guessing cannot destroy a job.
    fn redeem(&self, token: u32, caller: &PortalCaller) -> Option<Prepared> {
        let mut table = self.prepared.lock_recover();
        table.retain(|_, job| job.issued.elapsed() < TOKEN_TTL);
        if table.get(&token)?.caller != *caller {
            tracing::warn!(app = %caller.app_id, "print token of another app refused");
            return None;
        }
        table.remove(&token)
    }
}

/// The document behind the app's descriptor: a regular file only (a pipe
/// with no writer would block, a device may never end), read from its
/// start and at most [`MAX_SPOOL_BYTES`].
///
/// # Errors
/// It is no regular file, too large, empty, or unreadable.
pub fn spool_from_fd(fd: std::os::fd::OwnedFd) -> Result<Vec<u8>, String> {
    let mut file = std::fs::File::from(fd);
    let meta = file.metadata().map_err(|error| error.to_string())?;
    if !meta.is_file() {
        return Err("the document is not a regular file".into());
    }
    let max = u64::try_from(MAX_SPOOL_BYTES).unwrap_or(u64::MAX);
    if meta.len() > max {
        return Err("the document is too large to print".into());
    }
    let _ = file.seek(SeekFrom::Start(0));
    let mut document = Vec::new();
    file.take(max.saturating_add(1))
        .read_to_end(&mut document)
        .map_err(|error| error.to_string())?;
    if document.len() > MAX_SPOOL_BYTES {
        return Err("the document is too large to print".into());
    }
    if document.is_empty() {
        return Err("the document is empty".into());
    }
    Ok(document)
}

/// An app's title made safe for argv, the queue and a log: controls and
/// bidi overrides become spaces, leading dashes go, at most
/// [`MAX_TITLE_BYTES`]; `None` when nothing is left.
#[must_use]
pub fn sanitise_title(raw: &str) -> Option<String> {
    let mut out = String::new();
    for c in raw.chars() {
        let safe = if is_unsafe_title_char(c) { ' ' } else { c };
        if out.len().saturating_add(safe.len_utf8()) > MAX_TITLE_BYTES {
            break;
        }
        out.push(safe);
    }
    let trimmed = out.trim().trim_start_matches('-').trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn is_unsafe_title_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
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

/// A destination fit for argv: `[A-Za-z0-9._-]`, not leading `-`.
#[must_use]
pub fn valid_destination(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 127
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// A copy count in `1..=MAX_COPIES`.
#[must_use]
pub fn clamp_copies(requested: Option<u32>) -> u32 {
    requested.unwrap_or(1).clamp(1, MAX_COPIES)
}

/// The copy count of GTK-shaped settings: `n-copies` as text or a `u`.
fn copies_of(settings: &HashMap<String, OwnedValue>) -> Option<u32> {
    option::<u32>(settings, "n-copies")
        .or_else(|| option::<String>(settings, "n-copies")?.trim().parse().ok())
}

/// `PreparePrint`'s results: the settings as decided, the app's page setup
/// back as it sent it, and the token.
fn prepare_results(
    destination: &str,
    copies: u32,
    token: u32,
    page_setup: &HashMap<String, OwnedValue>,
) -> Results {
    let mut results = Results::new();
    let settings: HashMap<String, Value<'static>> = HashMap::from([
        ("printer".to_owned(), Value::new(destination.to_owned())),
        ("n-copies".to_owned(), Value::new(copies.to_string())),
    ]);
    if let Ok(value) = OwnedValue::try_from(Value::new(settings)) {
        results.insert("settings".into(), value);
    }
    let echoed: HashMap<String, Value<'static>> = page_setup
        .iter()
        .filter_map(|(key, value)| {
            value
                .try_clone()
                .ok()
                .map(|v| (key.clone(), Value::from(v)))
        })
        .collect();
    if let Ok(value) = OwnedValue::try_from(Value::new(echoed)) {
        results.insert("page-setup".into(), value);
    }
    results.insert("token".into(), OwnedValue::from(token));
    results
}

/// The `org.freedesktop.impl.portal.Print` interface.
pub struct Print {
    portal: Portal,
    spooler: Arc<dyn Spooler>,
    tokens: Tokens,
}

impl Print {
    #[must_use]
    pub fn new(portal: Portal, spooler: Arc<dyn Spooler>) -> Self {
        Self {
            portal,
            spooler,
            tokens: Tokens::default(),
        }
    }

    /// Ask the person which printer `title` goes to, offering the app's
    /// `requested` printer first; the chosen printer and copies.
    async fn choose(
        &self,
        caller: &PortalCaller,
        path: &str,
        title: String,
        requested: Option<&str>,
        copies: u32,
    ) -> Result<(String, u32), Ended> {
        let spooler = Arc::clone(&self.spooler);
        let printers = tokio::task::spawn_blocking(move || spooler.destinations())
            .await
            .unwrap_or_default();
        if printers.is_empty() {
            return Err(Ended::Failed);
        }
        let selected = requested
            .and_then(|want| printers.iter().position(|name| name == want))
            .unwrap_or(0);
        let question = PrintQuestion {
            title,
            printers: printers.clone(),
            selected,
            copies,
        };
        let answered = self
            .portal
            .ask(caller, path, PRINT_QUESTION, question)
            .await?;
        let chosen = printers
            .get(answered.choice.unwrap_or(selected))
            .filter(|name| valid_destination(name))
            .ok_or(Ended::Failed)?;
        let copies = answered
            .values
            .first()
            .and_then(|typed| typed.trim().parse().ok())
            .map_or(copies, |typed| clamp_copies(Some(typed)));
        Ok((chosen.clone(), copies))
    }
}

/// `PreparePrint`'s wire arguments: `handle o`, `app_id s`,
/// `parent_window s`, `title s`, `settings a{sv}`, `page_setup a{sv}`,
/// `options a{sv}`.
type PreparePrintArgs = (
    OwnedObjectPath,
    String,
    String,
    String,
    HashMap<String, OwnedValue>,
    HashMap<String, OwnedValue>,
    HashMap<String, OwnedValue>,
);

/// What `PreparePrint` reads of its arguments.
struct PreparePrintRequest {
    handle: OwnedObjectPath,
    app_id: String,
    parent_window: String,
    title: String,
    settings: HashMap<String, OwnedValue>,
    page_setup: HashMap<String, OwnedValue>,
}

impl From<PreparePrintArgs> for PreparePrintRequest {
    fn from(
        (handle, app_id, parent_window, title, settings, page_setup, _options): PreparePrintArgs,
    ) -> Self {
        Self {
            handle,
            app_id,
            parent_window,
            title,
            settings,
            page_setup,
        }
    }
}

/// `Print`'s wire arguments: `handle o`, `app_id s`, `parent_window s`,
/// `title s`, `fd h`, `options a{sv}`.
type PrintArgs = (
    OwnedObjectPath,
    String,
    String,
    String,
    OwnedFd,
    HashMap<String, OwnedValue>,
);

/// What `Print` reads of its arguments.
struct PrintRequest {
    handle: OwnedObjectPath,
    app_id: String,
    parent_window: String,
    title: String,
    fd: OwnedFd,
    options: HashMap<String, OwnedValue>,
}

impl From<PrintArgs> for PrintRequest {
    fn from((handle, app_id, parent_window, title, fd, options): PrintArgs) -> Self {
        Self {
            handle,
            app_id,
            parent_window,
            title,
            fd,
            options,
        }
    }
}

/// The interface's introspection, as zbus's `#[interface]` macro writes it.
const PRINT_INTROSPECTION: &str = r#"<interface name="org.freedesktop.impl.portal.Print">
  <method name="PreparePrint">
    <arg name="handle" type="o" direction="in"/>
    <arg name="app_id" type="s" direction="in"/>
    <arg name="parent_window" type="s" direction="in"/>
    <arg name="title" type="s" direction="in"/>
    <arg name="settings" type="a{sv}" direction="in"/>
    <arg name="page_setup" type="a{sv}" direction="in"/>
    <arg name="options" type="a{sv}" direction="in"/>
    <arg type="u" direction="out"/>
    <arg type="a{sv}" direction="out"/>
  </method>
  <method name="Print">
    <arg name="handle" type="o" direction="in"/>
    <arg name="app_id" type="s" direction="in"/>
    <arg name="parent_window" type="s" direction="in"/>
    <arg name="title" type="s" direction="in"/>
    <arg name="fd" type="h" direction="in"/>
    <arg name="options" type="a{sv}" direction="in"/>
    <arg type="u" direction="out"/>
    <arg type="a{sv}" direction="out"/>
  </method>
  <property name="version" type="u" access="read"/>
</interface>
"#;

dispatch::method_interface!(
    Print,
    "org.freedesktop.impl.portal.Print",
    3_u32,
    PRINT_INTROSPECTION
);

impl Print {
    fn dispatch_call<'call>(
        &'call self,
        connection: &'call Connection,
        message: &'call Message,
        name: &str,
    ) -> DispatchResult2<'call> {
        match name {
            "PreparePrint" => dispatch::dispatch(
                connection,
                message,
                move |header, args: PreparePrintArgs| async move {
                    Ok(self.prepare_print(&header, connection, args.into()).await)
                },
            ),
            "Print" => dispatch::dispatch(
                connection,
                message,
                move |header, args: PrintArgs| async move {
                    Ok(self.print(&header, connection, args.into()).await)
                },
            ),
            _ => DispatchResult2::NotFound,
        }
    }

    async fn prepare_print(
        &self,
        header: &Header<'_>,
        connection: &Connection,
        request: PreparePrintRequest,
    ) -> Reply {
        let PreparePrintRequest {
            handle,
            app_id,
            parent_window,
            title,
            settings,
            page_setup,
        } = request;
        answer(async {
            let path = handle.as_str();
            let auth = self
                .portal
                .authenticate(header, path, &app_id, Some(&parent_window))
                .await?;
            if lockdown::current_policy().disable_printing {
                return Err(Ended::Cancelled);
            }
            let requested = option::<String>(&settings, "printer");
            let copies = clamp_copies(copies_of(&settings));
            let title = sanitise_title(&title).unwrap_or_default();
            let choosing = self.choose(&auth.caller, path, title, requested.as_deref(), copies);
            let (destination, copies) = self
                .portal
                .requests
                .run(connection, path, auth.owner.as_str(), choosing)
                .await?;
            let token = self.tokens.mint(Prepared {
                caller: auth.caller,
                destination: destination.clone(),
                copies,
                issued: Instant::now(),
            });
            Ok(prepare_results(&destination, copies, token, &page_setup))
        })
        .await
    }

    async fn print(
        &self,
        header: &Header<'_>,
        connection: &Connection,
        request: PrintRequest,
    ) -> Reply {
        let PrintRequest {
            handle,
            app_id,
            parent_window,
            title,
            fd,
            options,
        } = request;
        answer(async {
            let path = handle.as_str();
            let auth = self
                .portal
                .authenticate(header, path, &app_id, Some(&parent_window))
                .await?;
            if lockdown::current_policy().disable_printing {
                return Err(Ended::Cancelled);
            }
            let title = sanitise_title(&title).unwrap_or_default();
            // A token that does not redeem is refused, never taken as no
            // token: a replayed one must not print to the default.
            let (destination, copies) = match option::<u32>(&options, "token") {
                None | Some(0) => {
                    let choosing = self.choose(&auth.caller, path, title.clone(), None, 1);
                    self.portal
                        .requests
                        .run(connection, path, auth.owner.as_str(), choosing)
                        .await?
                }
                Some(token) => {
                    let job = self
                        .tokens
                        .redeem(token, &auth.caller)
                        .ok_or(Ended::Failed)?;
                    (job.destination, job.copies)
                }
            };
            let spooler = Arc::clone(&self.spooler);
            let caller = auth.caller;
            let fd: std::os::fd::OwnedFd = fd.into();
            let submitted = tokio::task::spawn_blocking(move || {
                spooler.submit(PrintJob {
                    caller,
                    title,
                    destination,
                    copies,
                    document: spool_from_fd(fd)?,
                })
            })
            .await;
            match submitted {
                Ok(Ok(job)) => {
                    tracing::info!(job, "print job submitted");
                    Ok(Results::new())
                }
                Ok(Err(error)) => {
                    tracing::warn!(%error, "print job refused");
                    Err(Ended::Failed)
                }
                Err(error) => {
                    tracing::warn!(%error, "print job lost");
                    Err(Ended::Failed)
                }
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Type as _;

    #[test]
    fn the_introspection_advertises_the_arguments_decoded() {
        let signature = |method| dispatch::advertised_in_signature(PRINT_INTROSPECTION, method);
        assert_eq!(
            format!("({})", signature("PreparePrint")),
            PreparePrintArgs::SIGNATURE.to_string()
        );
        assert_eq!(
            format!("({})", signature("Print")),
            PrintArgs::SIGNATURE.to_string()
        );
    }

    #[test]
    fn titles_lose_controls_bidi_and_dashes() {
        assert_eq!(sanitise_title("a\nb").as_deref(), Some("a b"));
        assert_eq!(sanitise_title("--force").as_deref(), Some("force"));
        assert_eq!(
            sanitise_title("fdp\u{202E}.exe").as_deref(),
            Some("fdp .exe")
        );
        assert_eq!(sanitise_title(" \u{7} ").as_deref(), None);
        assert!(sanitise_title(&"é".repeat(300)).unwrap().len() <= MAX_TITLE_BYTES);
    }

    #[test]
    fn destinations_and_copies_are_bounded() {
        assert!(valid_destination("Office_LaserJet-2"));
        assert!(!valid_destination("-oraw"));
        assert!(!valid_destination("a b"));
        assert_eq!(clamp_copies(None), 1);
        assert_eq!(clamp_copies(Some(0)), 1);
        assert_eq!(clamp_copies(Some(u32::MAX)), MAX_COPIES);
        let gtk = HashMap::from([(
            "n-copies".to_owned(),
            OwnedValue::from(zbus::zvariant::Str::from("3")),
        )]);
        assert_eq!(copies_of(&gtk), Some(3));
    }

    #[test]
    fn a_token_prints_once_and_only_for_its_app() {
        let tokens = Tokens::default();
        let notes = PortalCaller::new("so.tilo.notes");
        let token = tokens.mint(Prepared {
            caller: notes.clone(),
            destination: "Office".into(),
            copies: 2,
            issued: Instant::now(),
        });
        assert_ne!(token, 0);
        assert!(
            tokens
                .redeem(token, &PortalCaller::new("so.tilo.mail"))
                .is_none()
        );
        assert_eq!(tokens.redeem(token, &notes).map(|job| job.copies), Some(2));
        assert!(tokens.redeem(token, &notes).is_none(), "used once");
    }

    #[test]
    fn only_a_regular_file_is_spooled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.pdf");
        std::fs::write(&path, b"%PDF-1.7").unwrap();
        let file = std::fs::File::open(&path).unwrap();
        assert_eq!(spool_from_fd(file.into()).unwrap(), b"%PDF-1.7");
        let (_read, write) = std::io::pipe().unwrap();
        assert!(spool_from_fd(write.into()).is_err());
        let empty = dir.path().join("empty");
        std::fs::write(&empty, b"").unwrap();
        assert!(spool_from_fd(std::fs::File::open(&empty).unwrap().into()).is_err());
    }
}
