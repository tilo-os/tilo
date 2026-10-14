//! Choosers for an app Seatbelt confines (macOS).
//!
//! A panel the app opened itself would run inside its sandbox: it would show
//! the disk, and the app could then open nothing it picked. So the runtime
//! that launched the app shows the panel instead, outside the sandbox, and
//! answers every picked path with a sandbox extension — the kernel-signed
//! token App Sandbox's own panels hand out. Consuming it lets exactly that
//! path through the app's profile, for the rest of the process's life. The
//! app says what kind of chooser it wants; it never names a path, so it can
//! be granted only what the person picked.
//!
//! One request and one answer per line over the socket the runtime passes in
//! [`PICKER_FD_ENV`], each an `application/x-www-form-urlencoded` record.

use std::ffi::{CStr, CString, c_char};
use std::io::{self, BufRead, BufReader, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use crate::portal::{
    FileFilter, LaunchChannel, LaunchRequest, MultiChannel, MultiOutcome, OpenFileChannel,
    OpenFileOutcome, PickKind, PickRequest, PortalChannel, PortalOutcome, open_selected_uri,
};

/// The inherited socket to the runtime's chooser, as a descriptor number.
pub const PICKER_FD_ENV: &str = "TILO_FILE_PICKER_FD";

/// The extension class a confined app's profile admits, for reading and
/// writing what was picked (App Sandbox's user-selected read-write).
pub const EXTENSION_CLASS: &str = "com.apple.app-sandbox.read-write";

/// The longest request or answer line either side reads.
const MAX_LINE: u64 = 64 * 1024;

unsafe extern "C" {
    fn sandbox_extension_issue_file(
        class: *const c_char,
        path: *const c_char,
        flags: u32,
    ) -> *mut c_char;
    fn sandbox_extension_consume(token: *const c_char) -> i64;
}

/// What the chooser answered: the picked paths, each with the extension
/// token that opens it to the app, or why there are none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Picked(Vec<Picked>),
    Cancelled,
    Failed(String),
}

/// One picked path and the extension tokens that open it: its own, and for a
/// Save destination also that of [`crate::portal::save_sibling`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picked {
    pub path: PathBuf,
    pub tokens: Vec<String>,
}

/// `request` as one line.
pub fn encode_request(request: &PickRequest) -> String {
    let mut record = url::form_urlencoded::Serializer::new(String::new());
    match &request.kind {
        PickKind::Open { multiple: false } => record.append_pair("kind", "open"),
        PickKind::Open { multiple: true } => record.append_pair("kind", "open-multiple"),
        PickKind::Directory if request.is_writable_directory() => {
            record.append_pair("kind", "writable-folder")
        }
        PickKind::Directory => record.append_pair("kind", "folder"),
        PickKind::Save { suggested_name } => record
            .append_pair("kind", "save")
            .append_pair("name", suggested_name),
    };
    record
        .append_pair("title", &request.title)
        .append_pair("accept", &request.accept_label);
    for glob in request.filters.iter().flat_map(|filter| &filter.globs) {
        record.append_pair("glob", glob);
    }
    record.finish()
}

/// The request `line` encodes.
///
/// # Errors
///
/// `InvalidData` for a line that names no chooser kind this crate shows.
pub fn decode_request(line: &str) -> io::Result<PickRequest> {
    let mut kind = None;
    let (mut title, mut accept, mut name) = (String::new(), String::new(), String::new());
    let mut filter = FileFilter::new("Files");
    for (key, value) in url::form_urlencoded::parse(line.trim_end().as_bytes()) {
        match &*key {
            "kind" => kind = Some(value.into_owned()),
            "title" => title = value.into_owned(),
            "accept" => accept = value.into_owned(),
            "name" => name = value.into_owned(),
            "glob" => filter = filter.glob(&value),
            _ => {}
        }
    }
    let request = match kind.as_deref() {
        Some("open") => PickRequest::open(&title, &accept),
        Some("open-multiple") => PickRequest::open_multiple(&title, &accept),
        Some("folder") => PickRequest::directory(&title, &accept),
        Some("writable-folder") => PickRequest::writable_directory(&title, &accept),
        Some("save") => PickRequest::save(&title, &accept, &name),
        _ => return Err(invalid("a chooser request names no known kind")),
    };
    Ok(if filter.globs.is_empty() {
        request
    } else {
        request.filter(filter)
    })
}

impl Answer {
    /// This answer as one line.
    pub fn encode(&self) -> String {
        let mut record = url::form_urlencoded::Serializer::new(String::new());
        match self {
            Answer::Picked(picked) => {
                record.append_pair("outcome", "picked");
                for item in picked {
                    record.append_pair("path", &item.path.to_string_lossy());
                    for token in &item.tokens {
                        record.append_pair("token", token);
                    }
                }
            }
            Answer::Cancelled => {
                record.append_pair("outcome", "cancelled");
            }
            Answer::Failed(reason) => {
                record
                    .append_pair("outcome", "failed")
                    .append_pair("reason", reason);
            }
        }
        record.finish()
    }

    /// The answer `line` encodes.
    ///
    /// # Errors
    ///
    /// `InvalidData` for a line that is not an answer.
    pub fn decode(line: &str) -> io::Result<Self> {
        let mut outcome = None;
        let mut reason = String::new();
        let mut picked: Vec<Picked> = Vec::new();
        for (key, value) in url::form_urlencoded::parse(line.trim_end().as_bytes()) {
            match &*key {
                "outcome" => outcome = Some(value.into_owned()),
                "reason" => reason = value.into_owned(),
                "path" => picked.push(Picked {
                    path: PathBuf::from(&*value),
                    tokens: Vec::new(),
                }),
                "token" => picked
                    .last_mut()
                    .ok_or_else(|| invalid("a token names no path"))?
                    .tokens
                    .push(value.into_owned()),
                _ => {}
            }
        }
        match outcome.as_deref() {
            Some("picked") if !picked.is_empty() => Ok(Answer::Picked(picked)),
            Some("cancelled") => Ok(Answer::Cancelled),
            Some("failed") => Ok(Answer::Failed(reason)),
            _ => Err(invalid("not a chooser answer")),
        }
    }
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}

fn read_line(reader: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut line = String::new();
    let read = io::Read::take(&mut *reader, MAX_LINE).read_line(&mut line)?;
    if read == 0 {
        return Ok(None);
    }
    if !line.ends_with('\n') {
        return Err(invalid("a chooser line is too long or truncated"));
    }
    Ok(Some(line))
}

/// Issue the extension that opens `path` to a confined app.
///
/// # Errors
///
/// The kernel's refusal, for example for a path that does not exist.
pub fn issue_extension(path: &Path) -> io::Result<String> {
    let class = CString::new(EXTENSION_CLASS).map_err(|_| invalid("extension class"))?;
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| invalid("path has a NUL"))?;
    // SAFETY: both arguments are NUL-terminated strings that outlive the
    // call; a non-null result is a malloc'd C string this function frees.
    unsafe {
        let token = sandbox_extension_issue_file(class.as_ptr(), path.as_ptr(), 0);
        if token.is_null() {
            return Err(io::Error::last_os_error());
        }
        let owned = CStr::from_ptr(token).to_string_lossy().into_owned();
        libc::free(token.cast());
        Ok(owned)
    }
}

fn consume_extension(token: &str) -> io::Result<()> {
    let token = CString::new(token).map_err(|_| invalid("token has a NUL"))?;
    // SAFETY: `token` is a NUL-terminated string that outlives the call.
    if unsafe { sandbox_extension_consume(token.as_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Serve one confined app's chooser requests on `stream` until the app
/// closes it. Each request is shown by `show` (which answers the picked
/// paths, or `None` when the person cancelled); every picked path is
/// answered with its extension token. A Save destination that does not exist
/// yet is created empty first, and it is granted with its one
/// [`crate::portal::save_sibling`], so the app can save by writing that and
/// renaming it over the destination; nothing else in the folder is granted.
///
/// # Errors
///
/// A read or write on `stream` that failed.
pub fn serve(
    stream: UnixStream,
    show: impl Fn(&PickRequest) -> Result<Option<Vec<PathBuf>>, String>,
) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    while let Some(line) = read_line(&mut reader)? {
        let answer = match decode_request(&line) {
            Ok(request) => answer(&request, show(&request)),
            Err(error) => Answer::Failed(error.to_string()),
        };
        writer.write_all(answer.encode().as_bytes())?;
        writer.write_all(b"\n")?;
    }
    Ok(())
}

fn answer(request: &PickRequest, shown: Result<Option<Vec<PathBuf>>, String>) -> Answer {
    let paths = match shown {
        Ok(Some(paths)) if !paths.is_empty() => paths,
        Ok(_) => return Answer::Cancelled,
        Err(reason) => return Answer::Failed(reason),
    };
    let mut picked = Vec::with_capacity(paths.len());
    for path in paths {
        let mut granted = vec![path.clone()];
        if request.suggested_name().is_some() {
            if let Err(error) = create_destination(&path) {
                return Answer::Failed(format!("could not create {}: {error}", path.display()));
            }
            match crate::portal::save_sibling(&path) {
                Some(sibling) => granted.push(sibling),
                None => return Answer::Failed(format!("{} names no file", path.display())),
            }
        }
        let mut tokens = Vec::with_capacity(granted.len());
        for grant in &granted {
            match issue_extension(grant) {
                Ok(token) => tokens.push(token),
                Err(error) => {
                    return Answer::Failed(format!("could not grant {}: {error}", grant.display()));
                }
            }
        }
        picked.push(Picked { path, tokens });
    }
    Answer::Picked(picked)
}

fn create_destination(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o644)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map(drop)
}

/// The chooser helper's whole life: read one request from stdin, show its
/// panel modally, and write the picked paths (with no tokens: the runtime
/// that asked grants them) to stdout. Run it as the main thread of a process
/// of its own; the runtime has no run loop to show a panel on.
///
/// # Errors
///
/// Reading the request or writing the answer failed.
pub fn run_chooser_helper() -> io::Result<()> {
    let line = read_line(&mut io::stdin().lock())?
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "no chooser request"))?;
    let answer = match decode_request(&line) {
        Ok(request) => match crate::macos::show_modal(&request) {
            Ok(Some(paths)) if !paths.is_empty() => Answer::Picked(
                paths
                    .into_iter()
                    .map(|path| Picked {
                        path,
                        tokens: Vec::new(),
                    })
                    .collect(),
            ),
            Ok(_) => Answer::Cancelled,
            Err(reason) => Answer::Failed(reason),
        },
        Err(error) => Answer::Failed(error.to_string()),
    };
    let mut stdout = io::stdout().lock();
    stdout.write_all(answer.encode().as_bytes())?;
    stdout.write_all(b"\n")
}

/// The question the runtime asks the person, once, before a confined app
/// first captures from a device: already in the person's language, since
/// the runtime resolves it from its catalogue before showing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Consent {
    pub message: String,
    pub detail: String,
    /// The allowing button; the refusing one comes first and is the default.
    pub allow: String,
    pub deny: String,
}

impl Consent {
    fn encode(&self) -> String {
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("message", &self.message)
            .append_pair("detail", &self.detail)
            .append_pair("allow", &self.allow)
            .append_pair("deny", &self.deny)
            .finish()
    }

    fn decode(line: &str) -> Option<Self> {
        let (mut message, mut detail, mut allow, mut deny) = (None, None, None, None);
        for (key, value) in url::form_urlencoded::parse(line.trim_end().as_bytes()) {
            let slot = match &*key {
                "message" => &mut message,
                "detail" => &mut detail,
                "allow" => &mut allow,
                "deny" => &mut deny,
                _ => continue,
            };
            *slot = Some(value.into_owned());
        }
        Some(Self {
            message: message?,
            detail: detail?,
            allow: allow?,
            deny: deny?,
        })
    }
}

/// The consent helper's whole life: read one [`Consent`] from stdin, ask it
/// in a native alert, and write `allow` or `deny` to stdout. Like
/// [`run_chooser_helper`], the main thread of a process of its own.
///
/// # Errors
///
/// Reading the question or writing the answer failed.
pub fn run_consent_helper() -> io::Result<()> {
    let line = read_line(&mut io::stdin().lock())?
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "no consent question"))?;
    let consent = Consent::decode(&line).ok_or_else(|| invalid("not a consent question"))?;
    let allowed = crate::macos::ask_modal(
        &consent.message,
        &consent.detail,
        &consent.allow,
        &consent.deny,
    )
    .map_err(io::Error::other)?;
    writeln!(
        io::stdout().lock(),
        "{}",
        if allowed { "allow" } else { "deny" }
    )
}

/// Ask `consent` through the helper `command` runs ([`run_consent_helper`]).
/// Anything but a clear `allow` is a refusal.
///
/// # Errors
///
/// The helper could not run.
pub fn ask_consent_in_helper(
    mut command: std::process::Command,
    consent: &Consent,
) -> Result<bool, String> {
    use std::process::Stdio;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| format!("the consent prompt could not start: {error}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        // A helper that exits without reading is a refusal, not an error.
        let _ = writeln!(stdin, "{}", consent.encode());
    }
    let output = child
        .wait_with_output()
        .map_err(|error| error.to_string())?;
    Ok(String::from_utf8_lossy(&output.stdout).trim() == "allow")
}

/// Show `request` through the chooser helper `command` runs
/// ([`run_chooser_helper`]): the `show` a runtime passes to [`serve`].
///
/// # Errors
///
/// The helper could not run, or answered nothing a chooser answers.
pub fn show_in_helper(
    mut command: std::process::Command,
    request: &PickRequest,
) -> Result<Option<Vec<PathBuf>>, String> {
    use std::process::Stdio;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| format!("the file chooser could not start: {error}"))?;
    let asked = child
        .stdin
        .take()
        .ok_or("the file chooser has no input")
        .map_err(str::to_string)
        .and_then(|mut stdin| {
            writeln!(stdin, "{}", encode_request(request)).map_err(|error| error.to_string())
        });
    let output = child
        .wait_with_output()
        .map_err(|error| error.to_string())?;
    asked?;
    let answer = Answer::decode(&String::from_utf8_lossy(&output.stdout))
        .map_err(|error| format!("the file chooser answered nothing usable: {error}"))?;
    match answer {
        Answer::Picked(picked) => Ok(Some(paths(picked))),
        Answer::Cancelled => Ok(None),
        Answer::Failed(reason) => Err(reason),
    }
}

/// The chooser of an app whose runtime passed it one ([`PICKER_FD_ENV`]).
/// The descriptor is taken once, by the first call.
pub(crate) fn from_env() -> Option<Arc<ConfinedFilePicker>> {
    static PICKER: std::sync::OnceLock<Option<Arc<ConfinedFilePicker>>> =
        std::sync::OnceLock::new();
    PICKER.get_or_init(|| take_env().map(Arc::new)).clone()
}

fn take_env() -> Option<ConfinedFilePicker> {
    let fd: i32 = std::env::var(PICKER_FD_ENV).ok()?.parse().ok()?;
    // SAFETY: fstat on any number is safe; it only reports whether it names
    // an open descriptor.
    let open = unsafe {
        let mut stat: libc::stat = std::mem::zeroed();
        libc::fstat(fd, &raw mut stat) == 0
    };
    if !open {
        return None;
    }
    // SAFETY: the runtime passed this descriptor for this purpose alone, and
    // nothing else in the process claims it.
    let stream = UnixStream::from(unsafe { OwnedFd::from_raw_fd(fd) });
    Some(ConfinedFilePicker {
        runtime: Arc::new(Mutex::new(BufReader::new(stream))),
    })
}

/// [`crate::FilePicker`] for a confined app: the runtime shows every chooser.
pub(crate) struct ConfinedFilePicker {
    runtime: Arc<Mutex<BufReader<UnixStream>>>,
}

impl ConfinedFilePicker {
    /// Ask the runtime, off the calling thread, and deliver `finish(answer)`.
    fn ask<T: Send + 'static>(
        &self,
        request: &PickRequest,
        finish: fn(Answer) -> T,
    ) -> Receiver<T> {
        let (tx, rx) = std::sync::mpsc::channel();
        let wake = tilo_rt::wake::WakeHandle::new();
        let runtime = Arc::clone(&self.runtime);
        let line = encode_request(request);
        let refused = tx.clone();
        let spawned =
            tilo_rt::background::Background::current().spawn("tilo-confined-chooser", move || {
                let answer = exchange(&runtime, &line).unwrap_or_else(|error| {
                    Answer::Failed(format!("the runtime's chooser failed: {error}"))
                });
                if tx.send(finish(granted(answer))).is_ok() {
                    wake.wake();
                }
            });
        if spawned.is_err() {
            let _ = refused.send(finish(Answer::Failed(
                "this host cannot run a chooser in the background".into(),
            )));
        }
        rx
    }
}

fn exchange(runtime: &Mutex<BufReader<UnixStream>>, line: &str) -> io::Result<Answer> {
    let mut runtime = runtime
        .lock()
        .map_err(|_| io::Error::other("an earlier chooser request panicked"))?;
    runtime.get_mut().write_all(line.as_bytes())?;
    runtime.get_mut().write_all(b"\n")?;
    let answer = read_line(&mut *runtime)?;
    drop(runtime);
    Answer::decode(
        &answer
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "the runtime hung up"))?,
    )
}

/// Consume every picked path's token; a path the app cannot be granted is
/// not handed to it.
fn granted(answer: Answer) -> Answer {
    let Answer::Picked(picked) = answer else {
        return answer;
    };
    for item in &picked {
        let consumed = if item.tokens.is_empty() {
            Err(invalid("no token"))
        } else {
            item.tokens
                .iter()
                .try_for_each(|token| consume_extension(token))
        };
        if let Err(error) = consumed {
            return Answer::Failed(format!(
                "{} could not be opened to this app: {error}",
                item.path.display()
            ));
        }
    }
    Answer::Picked(picked)
}

fn paths(picked: Vec<Picked>) -> Vec<PathBuf> {
    picked.into_iter().map(|item| item.path).collect()
}

impl crate::FilePicker for ConfinedFilePicker {
    fn pick(&self, request: PickRequest) -> PortalChannel {
        self.ask(&request, |answer| match answer {
            Answer::Picked(picked) => paths(picked)
                .into_iter()
                .next()
                .map_or(PortalOutcome::Cancelled, PortalOutcome::Opened),
            Answer::Cancelled => PortalOutcome::Cancelled,
            Answer::Failed(reason) => PortalOutcome::Failed(reason),
        })
    }

    fn pick_multiple(&self, request: PickRequest) -> MultiChannel {
        self.ask(&request, |answer| match answer {
            Answer::Picked(picked) => MultiOutcome::Selected(paths(picked)),
            Answer::Cancelled => MultiOutcome::Cancelled,
            Answer::Failed(reason) => MultiOutcome::Failed(reason),
        })
    }

    fn open_file(&self, request: PickRequest) -> OpenFileChannel {
        if !matches!(request.kind, PickKind::Open { multiple: false }) {
            let (tx, rx) = std::sync::mpsc::channel();
            let _ = tx.send(OpenFileOutcome::Failed(
                "an already-open file request must be a single-file Open chooser".to_string(),
            ));
            return rx;
        }
        self.ask(&request, |answer| match answer {
            Answer::Picked(picked) => {
                paths(picked)
                    .first()
                    .map_or(
                        OpenFileOutcome::Cancelled,
                        |path| match url::Url::from_file_path(path) {
                            Ok(uri) => open_selected_uri(&uri),
                            Err(()) => OpenFileOutcome::Failed(format!(
                                "not a local file: {}",
                                path.display()
                            )),
                        },
                    )
            }
            Answer::Cancelled => OpenFileOutcome::Cancelled,
            Answer::Failed(reason) => OpenFileOutcome::Failed(reason),
        })
    }

    fn launch(&self, request: LaunchRequest) -> LaunchChannel {
        crate::macos::PanelFilePicker.launch(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FilePicker as _;

    #[test]
    fn requests_and_answers_survive_the_wire() {
        let requests = [
            PickRequest::open("Open a\nbook", "Open & read"),
            PickRequest::open_multiple("Pick", "Add").filter(FileFilter::new("Img").glob("*.png")),
            PickRequest::directory("Folder", "Choose"),
            PickRequest::writable_directory("Import into folder", "Import here"),
            PickRequest::save("Save", "Save", "notes=1&x.txt"),
        ];
        for request in requests {
            let line = encode_request(&request);
            assert!(!line.contains('\n'));
            let decoded = decode_request(&line).unwrap();
            assert_eq!(encode_request(&decoded), line);
            assert_eq!(decoded.suggested_name(), request.suggested_name());
            assert_eq!(
                decoded.is_writable_directory(),
                request.is_writable_directory()
            );
        }
        assert!(decode_request("kind=format-disk").is_err());

        let answers = [
            Answer::Picked(vec![
                Picked {
                    path: PathBuf::from("/Users/u/a b&c.txt"),
                    tokens: vec!["t0k;en".into(), "sib&ling".into()],
                },
                Picked {
                    path: PathBuf::from("/Users/u/d"),
                    tokens: Vec::new(),
                },
            ]),
            Answer::Cancelled,
            Answer::Failed("no\nway".into()),
        ];
        for answer in answers {
            let line = answer.encode();
            assert!(!line.contains('\n'));
            assert_eq!(Answer::decode(&line).unwrap(), answer);
        }
        assert!(Answer::decode("outcome=picked").is_err());
    }

    #[test]
    fn the_runtime_grants_what_was_picked_and_nothing_when_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let existing = root.join("book.epub");
        std::fs::write(&existing, b"book").unwrap();
        let destination = root.join("new.txt");

        let (app, runtime) = UnixStream::pair().unwrap();
        let shown = {
            let (existing, destination) = (existing.clone(), destination.clone());
            move |request: &PickRequest| match request.suggested_name() {
                Some("cancel") => Ok(None),
                Some(_) => Ok(Some(vec![destination.clone()])),
                None => Ok(Some(vec![existing.clone()])),
            }
        };
        let server = std::thread::spawn(move || serve(runtime, shown));
        let picker = ConfinedFilePicker {
            runtime: Arc::new(Mutex::new(BufReader::new(app))),
        };

        assert_eq!(
            picker
                .pick(PickRequest::open("Open", "Open"))
                .recv()
                .unwrap(),
            PortalOutcome::Opened(existing)
        );
        assert_eq!(
            picker
                .pick(PickRequest::save("Save", "Save", "new.txt"))
                .recv()
                .unwrap(),
            PortalOutcome::Opened(destination.clone())
        );
        assert!(
            destination.is_file(),
            "the Save destination was not created"
        );
        assert_eq!(
            picker
                .pick(PickRequest::save("Save", "Save", "cancel"))
                .recv()
                .unwrap(),
            PortalOutcome::Cancelled
        );
        match picker
            .open_file(PickRequest::open("Open", "Open"))
            .recv()
            .unwrap()
        {
            OpenFileOutcome::Opened(file) => assert_eq!(file.display_name(), "book.epub"),
            other => panic!("{other:?}"),
        }
        drop(picker);
        server.join().unwrap().unwrap();
    }

    #[test]
    fn a_save_is_replaced_through_its_one_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("notes.txt");
        assert_eq!(
            crate::portal::save_sibling(&destination),
            Some(dir.path().join(".notes.txt.tilo-save"))
        );
        std::fs::write(&destination, b"old").unwrap();
        crate::portal::replace_saved(&destination, b"new").unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"new");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        assert!(crate::portal::save_sibling(Path::new("/")).is_none());
    }

    #[test]
    fn a_consent_question_survives_the_wire_and_only_allow_allows() {
        let consent = Consent {
            message: "Video & Co=1 möchte die Kamera verwenden?".into(),
            detail: "keeps\nthis".into(),
            allow: "Erlauben".into(),
            deny: "Nicht erlauben".into(),
        };
        assert_eq!(Consent::decode(&consent.encode()), Some(consent.clone()));
        assert_eq!(Consent::decode("message=x&allow=y"), None);
        let answer = |script: &str| {
            let mut command = std::process::Command::new("/bin/sh");
            command.args(["-c", script]);
            ask_consent_in_helper(command, &consent).unwrap()
        };
        assert!(answer("read q; echo allow"));
        assert!(!answer("read q; echo deny"));
        assert!(!answer("read q; echo allowed"));
        assert!(!answer("exit 1"));
    }

    #[test]
    fn a_path_that_cannot_be_granted_is_a_failure_not_a_path() {
        let request = PickRequest::open("Open", "Open");
        let missing = PathBuf::from("/nonexistent/tilo-chooser-test");
        assert!(matches!(
            answer(&request, Ok(Some(vec![missing]))),
            Answer::Failed(reason) if reason.contains("could not grant")
        ));
        assert!(matches!(
            granted(Answer::Picked(vec![Picked {
                path: PathBuf::from("/x"),
                tokens: Vec::new(),
            }])),
            Answer::Failed(_)
        ));
    }
}
