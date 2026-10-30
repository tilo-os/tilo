//! The clipboard's history: the copies a person can paste again, and the
//! read that fills it.
//!
//! A copy is kept as one format's bytes: text as UTF-8, or one binary
//! format (an image, a file list) as the app offered it. The history is
//! bounded and newest first; a copy the person pinned outlives clearing and
//! eviction, and pasting one again puts it back on the clipboard as if the
//! app that made it had copied it now.
//!
//! Secret copies never enter: password managers mark theirs ([`SECRET_MIME`],
//! [`PASSWORD_MANAGER_HINT_MIME`]), and a one-time code searched out of a
//! history is exactly what those markers exist to stop.
//!
//! Reading the selection is the loop's job, never a blocking one: a client
//! answers a read when it gets round to it, so [`Pull`] takes the bytes only
//! as they arrive, bounded, and gives up on a client that never answers.

use std::collections::VecDeque;
use std::io::Read as _;
use std::time::{Duration, Instant};

pub use crate::selection::TEXT_MIME;

/// Marks a Tilo copy as a secret: kept out of the history and never shared
/// with the person's other devices.
pub const SECRET_MIME: &str = "application/x-tilo-secret";

/// The password-manager convention other Linux clipboard managers honour: an
/// offer carrying it is kept out of their histories, and out of this one.
pub const PASSWORD_MANAGER_HINT_MIME: &str = "x-kde-passwordManagerHint";

/// The most copies the history keeps.
pub const MAX_COPIES: usize = 50;
/// The most bytes one kept copy carries.
pub const MAX_COPY_BYTES: usize = 256 * 1024;
/// How long a read waits for the app that owns the copy to answer.
pub const PULL_DEADLINE: Duration = Duration::from_secs(2);

/// The formats a history keeps, most preferred first: what a person copies
/// to paste again. A list of files comes before its text rendering, because
/// the files are what was copied; a file manager that offers both would
/// otherwise have its copy kept as the names it wrote for the screen.
const KEPT_MIMES: [&str; 7] = [
    "x-special/gnome-copied-files",
    "text/uri-list",
    TEXT_MIME,
    "image/png",
    "image/jpeg",
    "image/webp",
    "text/html",
];

/// Whether an offer carries a marker that forbids keeping it.
#[must_use]
pub fn is_secret(mimes: &[String]) -> bool {
    mimes
        .iter()
        .any(|mime| mime == SECRET_MIME || mime == PASSWORD_MANAGER_HINT_MIME)
}

/// The format the history keeps `mimes` in, most preferred first: what a
/// person copies to paste again. `None` for an offer worth remembering in
/// none of them, and for one marked secret.
#[must_use]
pub fn kept_mime(mimes: &[String]) -> Option<&'static str> {
    if is_secret(mimes) {
        return None;
    }
    KEPT_MIMES
        .iter()
        .copied()
        .find(|mime| mimes.iter().any(|offered| offered == mime))
}

/// The id of a copy of `bytes`: content-addressed, so the same copy made
/// twice is one copy, whoever the app that made it was.
fn id_of(mime: &str, bytes: &[u8]) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo-clipboard-copy-v1\0");
    hash.update(mime.as_bytes());
    hash.update(&[0]);
    hash.update(bytes);
    *hash.finalize().as_bytes()
}

/// One copy the person can paste again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Copy {
    /// Content-addressed: the same bytes copied twice are one copy.
    pub id: [u8; 32],
    /// The format [`Self::bytes`] are in.
    pub mime: String,
    pub bytes: Vec<u8>,
    /// When it was copied, in Unix milliseconds.
    pub created_ms: u64,
    /// Whether the person keeps it when the history is cleared.
    pub pinned: bool,
    /// The device it was shared from, when another of theirs sent it.
    pub from: Option<String>,
}

impl Copy {
    /// The copy of `mime`'s `bytes`, with its id taken from the bytes.
    #[must_use]
    pub fn new(mime: &str, bytes: Vec<u8>, created_ms: u64) -> Self {
        Self {
            id: id_of(mime, &bytes),
            mime: mime.to_owned(),
            bytes,
            created_ms,
            pinned: false,
            from: None,
        }
    }

    /// How a panel names this copy: what the person pressed in the history.
    #[must_use]
    pub fn key(&self) -> String {
        use std::fmt::Write as _;
        let mut key = String::with_capacity(64);
        for byte in self.id {
            let _ = write!(key, "{byte:02x}");
        }
        key
    }

    /// The copy a panel named as `key`, when it is [`Self::key`]'s.
    #[must_use]
    pub fn id_of_key(key: &str) -> Option<[u8; 32]> {
        if key.len() != 64 {
            return None;
        }
        let mut id = [0u8; 32];
        for (byte, pair) in id.iter_mut().zip(key.as_bytes().chunks_exact(2)) {
            *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
        }
        Some(id)
    }

    /// The whole text of a text copy, for pasting it into the focused field.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        if self.mime != TEXT_MIME {
            return None;
        }
        std::str::from_utf8(&self.bytes)
            .ok()
            .filter(|text| !text.is_empty())
    }

    /// The copy's bytes as text, whatever its format: a file list is read
    /// back with this.
    #[must_use]
    pub fn utf8(&self) -> Option<&str> {
        std::str::from_utf8(&self.bytes)
            .ok()
            .filter(|s| !s.is_empty())
    }

    /// The first line of a text copy, for the Clipboard section to list.
    #[must_use]
    pub fn text_preview(&self) -> Option<&str> {
        self.text()?
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
    }
}

/// The copies the person can paste again, newest first.
#[derive(Clone, Debug, Default)]
pub struct History {
    copies: VecDeque<Copy>,
}

impl History {
    /// Keep what the selection offered: `mimes` as they are offered, and the
    /// bytes of `mime`, the format [`kept_mime`] chose. Returns whether the
    /// history changed.
    ///
    /// A copy of the same bytes moves to the front and keeps what the
    /// person made of it (its pin), as a clipboard manager does.
    pub fn record(
        &mut self,
        mimes: &[String],
        mime: &str,
        bytes: Vec<u8>,
        created_ms: u64,
    ) -> bool {
        if is_secret(mimes) || bytes.is_empty() || bytes.len() > MAX_COPY_BYTES {
            return false;
        }
        if kept_mime(mimes).is_none_or(|kept| kept != mime) {
            return false;
        }
        let id = id_of(mime, &bytes);
        if let Some(index) = self.copies.iter().position(|copy| copy.id == id) {
            if index == 0 {
                return false;
            }
            if let Some(copy) = self.copies.remove(index) {
                self.copies.push_front(copy);
            }
            return true;
        }
        let copy = Copy::new(mime, bytes, created_ms);
        if !self.make_room() {
            return false;
        }
        self.copies.push_front(copy);
        true
    }

    /// Import a copy another of the person's devices shared.
    ///
    /// Returns whether the history changed. A copy of the same bytes already
    /// here keeps its own origin and place.
    pub fn import(&mut self, copy: Copy) -> bool {
        if self.copies.iter().any(|here| here.id == copy.id) {
            return false;
        }
        if !self.make_room() {
            return false;
        }
        // Newest first, like a copy made here: the person's next choice is
        // the one they made last, wherever they made it.
        self.copies.push_front(copy);
        true
    }

    /// Make room for one more copy, dropping the oldest unpinned one;
    /// `false` when every copy is pinned.
    fn make_room(&mut self) -> bool {
        while self.copies.len() >= MAX_COPIES {
            let Some(index) = self.copies.iter().rposition(|copy| !copy.pinned) else {
                return false;
            };
            self.copies.remove(index);
        }
        true
    }

    /// Every copy, newest first.
    pub fn copies(&self) -> impl Iterator<Item = &Copy> {
        self.copies.iter()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.copies.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.copies.is_empty()
    }

    #[must_use]
    pub fn get(&self, id: [u8; 32]) -> Option<&Copy> {
        self.copies.iter().find(|copy| copy.id == id)
    }

    /// The copy a panel named as `key` ([`Copy::key`]).
    #[must_use]
    pub fn find(&self, key: &str) -> Option<&Copy> {
        self.get(Copy::id_of_key(key)?)
    }

    /// The whole text of the text copy `id`.
    #[must_use]
    pub fn text(&self, id: [u8; 32]) -> Option<&str> {
        self.get(id)?.text()
    }

    /// Pin or unpin copy `id`: `false` when there is no such copy.
    pub fn pin(&mut self, id: [u8; 32], pinned: bool) -> bool {
        match self.copies.iter_mut().find(|copy| copy.id == id) {
            Some(copy) => {
                copy.pinned = pinned;
                true
            }
            None => false,
        }
    }

    /// Remove copy `id`: `false` when there is no such copy.
    pub fn remove(&mut self, id: [u8; 32]) -> bool {
        let before = self.copies.len();
        self.copies.retain(|copy| copy.id != id);
        self.copies.len() != before
    }

    /// Clear the history, keeping what the person pinned: how many went.
    pub fn clear(&mut self) -> usize {
        let before = self.copies.len();
        self.copies.retain(|copy| copy.pinned);
        before.saturating_sub(self.copies.len())
    }

    /// Take up copies kept by the settings store, newest first, replacing
    /// what is here.
    pub fn restore(&mut self, kept: impl IntoIterator<Item = Copy>) {
        self.copies = kept.into_iter().collect();
    }
}

/// Reading `selection` off the loop: the bytes arrive when the app that owns
/// it gets round to writing them.
pub struct Pull {
    reader: std::io::PipeReader,
    started: Instant,
    mime: String,
    bytes: Vec<u8>,
}

/// What one read of the selection has come to.
#[derive(Debug, PartialEq, Eq)]
pub enum Pulled {
    /// Nothing yet, or only part of it.
    Pending,
    /// All of it.
    Ready(Vec<u8>),
    /// The app let go, the reader died, or the copy is too big to keep.
    Lost,
}

impl Pull {
    /// Read what the owner of `mime` writes, up to [`MAX_COPY_BYTES`].
    ///
    /// The read is made non-blocking here, so a read only ever takes what
    /// has already arrived: the app that owns the copy answers when it gets
    /// round to it, and the loop must not wait for it.
    ///
    /// # Errors
    /// The read end could not be made non-blocking.
    pub fn new(reader: std::io::PipeReader, mime: &str) -> std::io::Result<Self> {
        rustix::fs::fcntl_setfl(&reader, rustix::fs::OFlags::NONBLOCK)
            .map_err(std::io::Error::from)?;
        Ok(Self {
            reader,
            started: Instant::now(),
            mime: mime.to_owned(),
            bytes: Vec::new(),
        })
    }

    /// The format asked for.
    #[must_use]
    pub fn mime(&self) -> &str {
        &self.mime
    }

    /// Take what has arrived; never blocks.
    pub fn drain(&mut self) -> Pulled {
        let mut chunk = [0u8; 16 * 1024];
        loop {
            match self.reader.read(&mut chunk) {
                Ok(0) => return Pulled::Ready(std::mem::take(&mut self.bytes)),
                Ok(read) => {
                    if let Some(arrived) = chunk.get(..read) {
                        self.bytes.extend_from_slice(arrived);
                    }
                    if self.bytes.len() > MAX_COPY_BYTES {
                        return Pulled::Lost;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    return if self.started.elapsed() > PULL_DEADLINE {
                        Pulled::Lost
                    } else {
                        Pulled::Pending
                    };
                }
                Err(_) => return Pulled::Lost,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the session does with an offer: keep the format the history
    /// prefers, or nothing when it keeps none.
    fn keep(history: &mut History, offer: &[(&str, &[u8])], created_ms: u64) -> bool {
        let mimes: Vec<String> = offer.iter().map(|(mime, _)| (*mime).to_owned()).collect();
        let Some(mime) = kept_mime(&mimes) else {
            return false;
        };
        let Some((_, bytes)) = offer.iter().find(|(name, _)| *name == mime) else {
            return false;
        };
        history.record(&mimes, mime, bytes.to_vec(), created_ms)
    }

    #[test]
    fn a_copy_of_the_same_bytes_is_one_copy_at_the_front_and_keeps_its_pin() {
        let mut history = History::default();
        assert!(keep(&mut history, &[(TEXT_MIME, b"one")], 1));
        assert!(keep(&mut history, &[(TEXT_MIME, b"two")], 2));
        let one = history
            .copies()
            .find(|copy| copy.text() == Some("one"))
            .expect("the first copy")
            .id;
        assert!(history.pin(one, true));

        assert!(keep(&mut history, &[(TEXT_MIME, b"one")], 3));
        let texts: Vec<&str> = history
            .copies()
            .map(|copy| std::str::from_utf8(&copy.bytes).expect("text"))
            .collect();
        assert_eq!(texts, ["one", "two"], "the copy moved to the front");
        assert!(
            history.copies().next().expect("the copy").pinned,
            "the pin belongs to the copy, so it moves with it"
        );
        assert!(
            !history.copies().nth(1).expect("the copy").pinned,
            "the other copy was never pinned"
        );
        assert!(
            !keep(&mut history, &[(TEXT_MIME, b"one")], 4),
            "a copy already at the front changes nothing"
        );
    }

    #[test]
    fn secrets_and_what_has_no_kept_format_never_enter() {
        let mut history = History::default();
        assert!(!keep(
            &mut history,
            &[(TEXT_MIME, b"hunter2"), (SECRET_MIME, b"secret")],
            1
        ));
        assert!(!keep(
            &mut history,
            &[
                (TEXT_MIME, b"hunter2"),
                (PASSWORD_MANAGER_HINT_MIME, b"secret")
            ],
            1
        ));
        assert!(!keep(&mut history, &[("application/x-weird", b"x")], 1));
        assert!(!keep(&mut history, &[(TEXT_MIME, b"")], 1));
        assert!(history.is_empty());
    }

    #[test]
    fn the_history_is_bounded_and_pins_outlive_clearing() {
        let mut history = History::default();
        for index in 0..MAX_COPIES + 5 {
            assert!(keep(
                &mut history,
                &[(TEXT_MIME, format!("copy {index}").as_bytes())],
                index as u64
            ));
        }
        assert_eq!(history.len(), MAX_COPIES);
        assert_eq!(
            history.copies().last().map(|copy| copy.created_ms),
            Some(5),
            "the oldest went"
        );
        let pinned = history.copies().last().expect("a copy").id;
        assert!(history.pin(pinned, true));
        assert_eq!(history.clear(), MAX_COPIES - 1);
        assert_eq!(history.len(), 1);
        assert_eq!(history.copies().next().map(|copy| copy.id), Some(pinned));
        assert!(history.remove(pinned));
        assert!(history.is_empty());
    }

    #[test]
    fn a_text_copy_says_its_first_line_and_a_shared_one_keeps_where_it_came_from() {
        let mut history = History::default();
        keep(&mut history, &[(TEXT_MIME, b"hello\nworld")], 1);
        let text = history.copies().next().expect("a copy").id;
        assert_eq!(history.text(text), Some("hello\nworld"));
        assert_eq!(
            history.copies().next().expect("a copy").text_preview(),
            Some("hello")
        );

        let mut image = Copy::new("image/png", vec![0x89, b'P', b'N', b'G'], 2);
        image.from = Some("phone".into());
        assert!(history.import(image.clone()));
        assert!(!history.import(image.clone()), "one copy is one copy");
        assert_eq!(history.len(), 2);
        assert_eq!(history.text(image.id), None);
        assert_eq!(
            history
                .copies()
                .find(|copy| copy.id == image.id)
                .and_then(|copy| copy.from.as_deref()),
            Some("phone"),
            "the copy says where it came from"
        );
    }

    #[test]
    fn a_read_arrives_as_it_is_written_and_never_blocks() {
        use std::io::Write as _;
        let (reader, mut writer) = std::io::pipe().expect("pipe");
        let mut pull = Pull::new(reader, TEXT_MIME).expect("a non-blocking read");
        assert_eq!(pull.drain(), Pulled::Pending, "nothing written yet");
        writer.write_all(b"half").expect("write");
        writer.write_all(b" and half").expect("write");
        drop(writer);
        let mut complete = Vec::new();
        for _ in 0..100 {
            match pull.drain() {
                Pulled::Ready(bytes) => {
                    complete = bytes;
                    break;
                }
                Pulled::Pending => std::thread::sleep(Duration::from_millis(5)),
                Pulled::Lost => panic!("the copy was lost"),
            }
        }
        assert_eq!(complete, b"half and half");
        assert_eq!(pull.mime(), TEXT_MIME);
    }
}
