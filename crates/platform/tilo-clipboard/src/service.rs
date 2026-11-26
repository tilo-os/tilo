//! The clipboard as a service: an app's copies and pastes over the connection its session
//! opened for it, answered by the process that holds the person's selection.
//!
//! An app runs in a process of its own, so the selection is not there to offer or read: the
//! session holds it, and this is the wire between the two. That wire is the one an app reaches
//! the clipboard through in either language — [`Connected`] for tilolang's runtime and
//! [`ServiceClipboard`] for a Rust `ui` app — so a copy an app makes is the person's copy
//! wherever it was made. Every message is one length-prefixed text header and, for each format
//! whose bytes it carries, one length-prefixed body after it — the framing every socket in the
//! tree uses ([`wire_frame`]).
//!
//! A side is whatever answers on the session's end ([`ClipboardSide`]): the session's own
//! clipboard, so what an app copies is the person's copy (it lands in their history and on every
//! app's clipboard) and what it pastes is what they copied anywhere.

use std::io::Write as _;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tilo_socket::Stream;

use wire_frame::Framing;

use crate::{
    ClipboardBackend, ClipboardImage, ClipboardTarget, CopyContent,
    MAX_CLIPBOARD_IMAGE_INPUT_BYTES, MAX_CLIPBOARD_PLAIN_FALLBACK_BYTES, MAX_PASTED_IMAGE_BYTES,
    RichTextOffer,
};

/// The descriptor the launcher hands an app it starts (`TILO_SVC_CLIPBOARD_FD`): the app's end of
/// the connection served here.
pub const FD_ENV: &str = "TILO_SVC_CLIPBOARD_FD";

/// A request's header: `copy <target> <plain|secret> <n> <mime>…`,
/// `paste <target> <n> <type>…`, or `quit`.
const HEADER: Framing = Framing::little_endian(4096);
/// The bytes one copy offers or one paste answers: an image as copied anywhere, held to the bound
/// the clipboard itself sets for one taken in.
const BODY: Framing = Framing::little_endian(MAX_CLIPBOARD_IMAGE_INPUT_BYTES);
/// The longest text one copy or paste carries.
const TEXT: usize = MAX_CLIPBOARD_PLAIN_FALLBACK_BYTES;

/// Which selection a copy or paste is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// Ctrl+C and Ctrl+V.
    Clipboard,
    /// Select, then middle-click.
    Primary,
}

impl Target {
    fn name(self) -> &'static str {
        match self {
            Self::Clipboard => "clipboard",
            Self::Primary => "primary",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        match word {
            "clipboard" => Some(Self::Clipboard),
            "primary" => Some(Self::Primary),
            _ => None,
        }
    }
}

/// One copy's media types and their bytes.
type Formats = Vec<(String, Vec<u8>)>;

/// What answers on the session's end of an app's clipboard connection: the selection the
/// person's device holds.
pub trait ClipboardSide: Send + Sync {
    /// Offer `formats` — one copy's media types and their bytes — as the person's copy on
    /// `target`. `secret` marks a copy that histories and device sync must not keep. False when
    /// the selection could not take it.
    fn copy(&self, target: Target, secret: bool, formats: &[(String, Vec<u8>)]) -> bool;

    /// What the selection holds of the `types` it offers on `target`, each requested type it
    /// holds with its bytes. Empty when it holds none of them.
    fn paste(&self, target: Target, types: &[&str]) -> Formats;
}

/// Serve an app's clipboard requests on `stream` until it closes the connection.
pub fn serve(stream: Stream, side: &dyn ClipboardSide) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = stream;
    loop {
        let Ok(Some(header)) = HEADER.read(&mut reader) else {
            return;
        };
        let Ok(text) = String::from_utf8(header) else {
            return;
        };
        let mut words = text.split_whitespace();
        match words.next() {
            Some("copy") => {
                let Some((target, secret, formats)) = copy_request(&mut words, &mut reader) else {
                    return;
                };
                let answer: &[u8] = if side.copy(target, secret, &formats) {
                    b"ok"
                } else {
                    b"no"
                };
                if HEADER.write(&mut writer, answer).is_err() || writer.flush().is_err() {
                    return;
                }
            }
            Some("paste") => {
                let Some((target, types)) = paste_request(&mut words) else {
                    return;
                };
                let types: Vec<&str> = types.iter().map(String::as_str).collect();
                let held = side.paste(target, &types);
                let mut header = format!("pasted {}", held.len());
                for (mime, bytes) in &held {
                    header.push(' ');
                    header.push_str(mime);
                    header.push(' ');
                    header.push_str(&bytes.len().to_string());
                }
                let wrote = HEADER.write(&mut writer, header.as_bytes()).and_then(|()| {
                    held.iter()
                        .try_for_each(|(_, bytes)| BODY.write(&mut writer, bytes))
                });
                if wrote.is_err() || writer.flush().is_err() {
                    return;
                }
            }
            _ => return,
        }
    }
}

/// The most formats one copy or paste may name: a rich copy offers two, and a paste asks for the
/// types it reads, so a request naming far more is not one an app makes.
const MAX_FORMATS: usize = 8;

/// The target, secrecy and formats of a copy request, its bodies read from `reader`.
fn copy_request(
    words: &mut std::str::SplitWhitespace<'_>,
    reader: &mut Stream,
) -> Option<(Target, bool, Formats)> {
    let target = Target::parse(words.next()?)?;
    let secret = match words.next()? {
        "secret" => true,
        "plain" => false,
        _ => return None,
    };
    let count: usize = words.next()?.parse().ok()?;
    if count > MAX_FORMATS {
        return None;
    }
    let mut formats = Vec::with_capacity(count);
    for _ in 0..count {
        let mime = words.next()?.to_owned();
        let body = BODY.read(reader).ok()??;
        if body.len() > most_of(&mime) {
            return None;
        }
        formats.push((mime, body));
    }
    Some((target, secret, formats))
}

/// The target and wanted types of a paste request.
fn paste_request(words: &mut std::str::SplitWhitespace<'_>) -> Option<(Target, Vec<String>)> {
    let target = Target::parse(words.next()?)?;
    let count: usize = words.next()?.parse().ok()?;
    if count > MAX_FORMATS {
        return None;
    }
    let types = words.take(count).map(str::to_owned).collect();
    Some((target, types))
}

/// The most a copy may offer of `mime`: a text is held to what text is, an image to what an image
/// taken in may be.
#[must_use]
pub fn most_of(mime: &str) -> usize {
    if mime.starts_with("text/") {
        TEXT
    } else {
        MAX_CLIPBOARD_IMAGE_INPUT_BYTES
    }
}

/// The media types a paste of an image asks for, in the order the device's clipboard is asked:
/// the types an image copied anywhere (a screenshot, a browser, a viewer) is offered as.
const IMAGE_TYPES: [&str; 3] = ["image/png", "image/jpeg", "image/webp"];

/// How long an app waits for the session's answer: a paste waits on the app that owns the
/// selection, which answers when it gets round to it, and never for good.
const WAIT: Duration = Duration::from_secs(30);

/// An app's end of the clipboard connection: what its runtime copies and pastes through.
pub struct Connected {
    writer: Mutex<Stream>,
    reader: Mutex<Stream>,
}

fn held<T>(lock: Result<T, std::sync::PoisonError<T>>) -> T {
    lock.unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Connected {
    /// The clipboard over `stream`, the connection the session opened for the app; none when the
    /// connection cannot be read or written.
    #[must_use]
    pub fn connect(stream: Stream) -> Option<Self> {
        let reader = stream.try_clone().ok()?;
        let _ = reader.set_read_timeout(Some(WAIT));
        let _ = reader.set_write_timeout(Some(WAIT));
        Some(Self {
            reader: Mutex::new(reader),
            writer: Mutex::new(stream),
        })
    }

    /// Offer `bytes` of `mime` as the person's copy: false when the session did not take it.
    #[must_use]
    pub fn copy(&self, mime: &str, bytes: &[u8]) -> bool {
        self.copy_formats(Target::Clipboard, false, &[(mime, bytes)])
    }

    /// Offer `text` as a secret copy (a password, a token, a key) that histories and device sync
    /// must not keep, and take it off the clipboard after `clear_after` unless the person copied
    /// something else meanwhile, so a later copy is never destroyed: what a Rust app's
    /// `ui::try_clipboard_copy_secret` does, for tilolang's runtime. False when the session did
    /// not take it.
    #[must_use]
    pub fn copy_secret(self: &Arc<Self>, text: &str, clear_after: Duration) -> bool {
        if !self.copy_formats(Target::Clipboard, true, &[("text/plain", text.as_bytes())]) {
            return false;
        }
        let clipboard = Arc::clone(self);
        let secret = text.to_owned();
        let cleared = std::thread::Builder::new()
            .name("clipboard-secret-clear".to_owned())
            .spawn(move || {
                std::thread::sleep(clear_after);
                let held = clipboard.paste(&["text/plain"]);
                if held.is_some_and(|(_, bytes)| bytes == secret.as_bytes()) {
                    let _ = clipboard.copy_formats(Target::Clipboard, true, &[("text/plain", b"")]);
                }
            });
        // A clear that cannot be scheduled must not leave the secret behind: take it back now.
        if cleared.is_err() {
            let _ = self.copy_formats(Target::Clipboard, true, &[("text/plain", b"")]);
            return false;
        }
        true
    }

    /// Offer `formats` as the person's copy on `target`, marked secret for a copy histories and
    /// device sync must not keep: false when the session did not take it, or a format is larger
    /// than the clipboard holds.
    #[must_use]
    pub fn copy_formats(&self, target: Target, secret: bool, formats: &[(&str, &[u8])]) -> bool {
        if formats
            .iter()
            .any(|(mime, bytes)| bytes.len() > most_of(mime))
        {
            return false;
        }
        let mimes: Vec<&str> = formats.iter().map(|(mime, _)| *mime).collect();
        let header = format!(
            "copy {} {} {} {}",
            target.name(),
            if secret { "secret" } else { "plain" },
            formats.len(),
            mimes.join(" ")
        );
        let mut writer: MutexGuard<'_, Stream> = held(self.writer.lock());
        let sent = HEADER
            .write(&mut *writer, header.as_bytes())
            .and_then(|()| {
                formats
                    .iter()
                    .try_for_each(|(_, bytes)| BODY.write(&mut *writer, bytes))
            });
        if sent.is_err() || writer.flush().is_err() {
            return false;
        }
        drop(writer);
        let mut reader = held(self.reader.lock());
        matches!(HEADER.read(&mut *reader), Ok(Some(answer)) if answer.as_slice() == b"ok")
    }

    /// What the selection holds of the first of `types` it offers, as its media type and bytes.
    #[must_use]
    pub fn paste(&self, types: &[&str]) -> Option<(String, Vec<u8>)> {
        self.paste_formats(Target::Clipboard, types)
            .into_iter()
            .next()
    }

    /// What the selection holds of each of `types` it offers on `target`, each with its bytes.
    #[must_use]
    pub fn paste_formats(&self, target: Target, types: &[&str]) -> Formats {
        let header = format!(
            "paste {} {} {}",
            target.name(),
            types.len(),
            types.join(" ")
        );
        let mut writer = held(self.writer.lock());
        if HEADER.write(&mut *writer, header.as_bytes()).is_err() || writer.flush().is_err() {
            return Vec::new();
        }
        drop(writer);
        let mut reader = held(self.reader.lock());
        let Ok(Some(reply)) = HEADER.read(&mut *reader) else {
            return Vec::new();
        };
        let Ok(reply) = String::from_utf8(reply) else {
            return Vec::new();
        };
        let mut words = reply.split_whitespace();
        if words.next() != Some("pasted") {
            return Vec::new();
        }
        let Some(count) = words.next().and_then(|count| count.parse().ok()) else {
            return Vec::new();
        };
        if count > MAX_FORMATS {
            return Vec::new();
        }
        let mut pasted = Vec::with_capacity(count);
        for _ in 0..count {
            let (Some(mime), Some(limit)) = (
                words.next().map(str::to_owned),
                words.next().and_then(|limit| limit.parse::<usize>().ok()),
            ) else {
                return Vec::new();
            };
            let Ok(Some(bytes)) = BODY.read(&mut *reader) else {
                return Vec::new();
            };
            // The header bounds what is read: a body of another length is not the paste it named.
            if bytes.len() != limit {
                return Vec::new();
            }
            pasted.push((mime, bytes));
        }
        drop(reader);
        pasted
    }
}

/// An app's clipboard as its `ClipboardBackend`: a Rust `ui` app's copies and pastes over the
/// session's connection, the same service [`Connected`] reaches in tilolang.
pub struct ServiceClipboard(Connected);

impl ServiceClipboard {
    /// The clipboard over the connection the session opened for this app, named by [`FD_ENV`];
    /// none when this process was handed none.
    #[cfg(unix)]
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let fd = std::env::var(FD_ENV).ok()?.parse::<i32>().ok()?;
        if fd < 0 {
            return None;
        }
        // SAFETY: the launcher passes this descriptor to this process, and it is this process's
        // from here: nothing else in the app holds the number.
        let stream = Stream::from(unsafe {
            <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd)
        });
        Some(Self(Connected::connect(stream)?))
    }
}

impl ClipboardBackend for ServiceClipboard {
    fn set_text(&self, target: ClipboardTarget, text: &str) -> bool {
        self.0.copy_formats(
            service_target(target),
            false,
            &[("text/plain", text.as_bytes())],
        )
    }

    fn text(&self, target: ClipboardTarget) -> Option<String> {
        let (_, bytes) = self
            .0
            .paste_formats(service_target(target), &["text/plain"])
            .into_iter()
            .next()?;
        String::from_utf8(bytes).ok()
    }

    fn request_text(&self, target: ClipboardTarget) -> std::sync::mpsc::Receiver<Option<String>> {
        let (reply, result) = std::sync::mpsc::channel();
        let _ = reply.send(self.text(target));
        result
    }

    fn set_shared_text(&self, target: ClipboardTarget, text: &str) -> bool {
        // Every copy through the service is one other processes read.
        self.set_text(target, text)
    }

    fn set_secret_text(&self, text: &str) -> bool {
        self.0
            .copy_formats(Target::Clipboard, true, &[("text/plain", text.as_bytes())])
    }

    fn set_rich_text(&self, html: &str, plain: &str) -> bool {
        self.0.copy_formats(
            Target::Clipboard,
            false,
            &[
                ("text/html", html.as_bytes()),
                ("text/plain", plain.as_bytes()),
            ],
        )
    }

    fn rich_text(&self) -> Option<RichTextOffer> {
        let held = self
            .0
            .paste_formats(Target::Clipboard, &["text/html", "text/plain"]);
        let html = held
            .iter()
            .find(|(mime, _)| mime == "text/html")
            .map(|(_, bytes)| String::from_utf8_lossy(bytes).into_owned())?;
        let plain = held
            .iter()
            .find(|(mime, _)| mime == "text/plain")
            .map(|(_, bytes)| String::from_utf8_lossy(bytes).into_owned());
        Some(RichTextOffer { html, plain }).filter(RichTextOffer::is_valid)
    }

    fn set_image(&self, width: usize, height: usize, rgba: &[u8]) -> bool {
        let Some(png) = crate::images::encode_png(width, height, rgba) else {
            return false;
        };
        self.0
            .copy_formats(Target::Clipboard, false, &[("image/png", &png)])
    }

    fn image(&self) -> Option<ClipboardImage> {
        let (mime, bytes) = self
            .0
            .paste_formats(Target::Clipboard, &IMAGE_TYPES)
            .into_iter()
            .next()?;
        if bytes.len() > MAX_PASTED_IMAGE_BYTES {
            return None;
        }
        Some(ClipboardImage {
            mime: IMAGE_TYPES
                .into_iter()
                .find(|known| *known == mime)
                .unwrap_or(IMAGE_TYPES[0]),
            bytes,
        })
    }

    fn copy(&self, content: CopyContent<'_>) -> crate::ClipboardCopy {
        crate::copy_now(self, content)
    }

    fn name(&self) -> &'static str {
        "session"
    }
}

fn service_target(target: ClipboardTarget) -> Target {
    match target {
        ClipboardTarget::Clipboard => Target::Clipboard,
        ClipboardTarget::Primary => Target::Primary,
    }
}

fn clipboard_target(target: Target) -> ClipboardTarget {
    match target {
        Target::Clipboard => ClipboardTarget::Clipboard,
        Target::Primary => ClipboardTarget::Primary,
    }
}

/// The service's answer where no Tilo session serves the clipboard: an app's copies and pastes
/// over the operating system's own clipboard, served by the process that started the app (the
/// launcher, or `tilolang run`). An app still talks only to [`Connected`] either way.
pub struct BackendSide {
    backend: Arc<dyn ClipboardBackend>,
}

impl BackendSide {
    /// The service side of `backend`: the operating system's clipboard, or a test's.
    #[must_use]
    pub fn new(backend: Arc<dyn ClipboardBackend>) -> Self {
        Self { backend }
    }

    fn text_of<'a>(formats: &'a [(String, Vec<u8>)], mime: &str) -> Option<&'a str> {
        formats
            .iter()
            .find(|(held, _)| held == mime)
            .and_then(|(_, bytes)| std::str::from_utf8(bytes).ok())
    }
}

impl ClipboardSide for BackendSide {
    fn copy(&self, target: Target, secret: bool, formats: &[(String, Vec<u8>)]) -> bool {
        if secret {
            // Histories and device sync are told not to keep a secret by the marker the OS
            // backend offers beside the words; outside a Tilo session the OS clipboard's own
            // conventions apply.
            return Self::text_of(formats, "text/plain")
                .is_some_and(|text| self.backend.set_secret_text(text));
        }
        if let (Some(html), Some(plain)) = (
            Self::text_of(formats, "text/html"),
            Self::text_of(formats, "text/plain"),
        ) {
            return self.backend.set_rich_text(html, plain);
        }
        let Some((mime, bytes)) = formats.first() else {
            return false;
        };
        if mime.starts_with("image/") {
            if target != Target::Clipboard {
                return false;
            }
            let Some((width, height, rgba)) = crate::images::decode_image(bytes) else {
                return false;
            };
            return self.backend.set_image(width, height, &rgba);
        }
        if mime == "text/plain" {
            let Ok(text) = std::str::from_utf8(bytes) else {
                return false;
            };
            return self.backend.set_text(clipboard_target(target), text);
        }
        false
    }

    fn paste(&self, target: Target, types: &[&str]) -> Formats {
        let target = clipboard_target(target);
        let mut held = Vec::new();
        for want in types {
            match *want {
                "text/plain" => {
                    if let Some(text) = self.backend.text(target)
                        && !text.is_empty()
                        && text.len() <= most_of("text/plain")
                    {
                        held.push(((*want).to_owned(), text.into_bytes()));
                    }
                }
                "text/html" => {
                    if let Some(offer) = self.backend.rich_text()
                        && offer.html.len() <= most_of("text/html")
                    {
                        held.push(((*want).to_owned(), offer.html.into_bytes()));
                    }
                }
                mime if mime.starts_with("image/") => {
                    let Some(image) = self.backend.image() else {
                        continue;
                    };
                    if image.mime == mime
                        && image.bytes.len() <= most_of(mime)
                        && image.bytes.len() <= MAX_PASTED_IMAGE_BYTES
                    {
                        held.push(((*want).to_owned(), image.bytes));
                    }
                }
                _ => {}
            }
        }
        held
    }
}

/// The clipboard connection an app is handed, the one service a Rust app and a tilolang instance
/// both reach their copy and paste through: the session's, connected at `session_socket`, where a
/// session serves one, so a copy is the person's copy on their other devices too. Where none does
/// (the person's own desktop, or a dev run outside a session), this process serves the same wire
/// from the operating system's clipboard ([`crate::os`]) on a thread that lives as long as the
/// app. `None` when neither can be had, and the app's copies and pastes are then refused.
#[cfg(any(unix, windows))]
#[must_use]
pub fn for_app(session_socket: &std::path::Path) -> Option<Stream> {
    if session_socket.exists() {
        return connect_session(session_socket);
    }
    serve_os()
}

/// The session's clipboard at `path`; `None` where nothing answers there.
#[cfg(any(unix, windows))]
#[must_use]
pub fn connect_session(path: &std::path::Path) -> Option<Stream> {
    if !path.exists() {
        return None;
    }
    match tilo_socket::connect(path) {
        Ok(stream) => Some(stream),
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "clipboard service refused connection");
            None
        }
    }
}

/// The app's end of a connection this process answers from the operating system's clipboard.
#[cfg(any(unix, windows))]
#[must_use]
pub fn serve_os() -> Option<Stream> {
    let (app, serving) = tilo_socket::pair().ok()?;
    std::thread::Builder::new()
        .name("tilo-clipboard-os".into())
        .spawn(move || {
            let side = BackendSide::new(crate::os());
            serve(serving, &side);
        })
        .ok()?;
    Some(app)
}
