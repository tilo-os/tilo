//! The emoji picker's session half: the caret of the field the person types
//! into, the emoji they pick written into that field, and the recents kept
//! between sessions.
//!
//! The picker itself is desktop chrome ([`tilo_desktop::emoji_picker`]): the
//! card, its search, its groups, its skin tones. It never writes into an
//! application. The chrome hands what was picked to its [`EmojiLink`], and
//! this module — beside dictation the one place the session writes text the
//! person did not type — commits each pick through the compositor's text
//! input, says what arrived and whether that field keeps its content secret,
//! and says where the focused field's caret stands so the card opens against
//! it.

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use emoji::Recents;
use tilo_compositor::Compositor;
use tilo_desktop::emoji_picker::{EmojiLink, EmojiPlace, RecentsStore};
use ui::kurbo::Rect;

/// How long a pick waits for the field it belongs to to take text again: the
/// card takes the keyboard while it is open, so the field it was opened from
/// gets it back only once the card has closed.
const DELIVERY_WINDOW: Duration = Duration::from_secs(2);

/// A pick waiting to reach a field.
struct Pending {
    text: String,
    /// The moment the pick gives up.
    deadline: Instant,
}

/// The emoji picker as the session hosts it: the chrome's link, and the pick
/// on its way into the field it was chosen from.
pub(crate) struct Emoji {
    link: EmojiLink,
    /// At most one: a newer pick replaces one that has not arrived yet.
    pending: RefCell<Option<Pending>>,
}

impl Emoji {
    /// Mount the picker's session half, keeping the recents where this
    /// user's state directory says.
    pub(crate) fn mount() -> Self {
        let link = FileRecentsStore::for_user()
            .map_or_else(EmojiLink::default, |store| EmojiLink::new(Box::new(store)));
        Self {
            link,
            pending: RefCell::new(None),
        }
    }

    /// The chrome's end of the link: what it opens against and what it picks.
    pub(crate) fn link(&self) -> EmojiLink {
        self.link.clone()
    }

    /// Carry the person's picks into the field they were typing in, and tell
    /// the chrome where that field's caret stands.
    ///
    /// `origin` is where the desk's own coordinates begin on the desktop,
    /// which the chrome's are measured from.
    pub(crate) fn step(&self, compositor: &mut Compositor, origin: (i32, i32), now: Instant) {
        for text in self.link.take_picks() {
            *self.pending.borrow_mut() = Some(Pending {
                text,
                deadline: now.checked_add(DELIVERY_WINDOW).unwrap_or(now),
            });
        }
        self.deliver(compositor, now);
        self.link.place_at(Self::place(compositor, origin));
    }

    /// Write the pick in flight into the focused field, which reports back
    /// whether it keeps its content secret. A field that does not take text
    /// yet is asked again next pass, until the pick's window has passed.
    fn deliver(&self, compositor: &mut Compositor, now: Instant) {
        let Some(pending) = self.pending.borrow_mut().take() else {
            return;
        };
        if let Some(sensitive) = compositor.insert_active_text(&pending.text) {
            self.link.note_delivered(pending.text, sensitive);
        } else if now < pending.deadline {
            *self.pending.borrow_mut() = Some(pending);
        } else {
            tracing::debug!("no field took the picked emoji in time");
        }
    }

    /// Where the card opens: the caret of the field the person types into and
    /// the display it stands on, in the chrome's coordinates. A field that
    /// reports no caret, or whose caret stands on no known display, leaves
    /// the display to the chrome, which centres the card on the one the
    /// person is working on.
    fn place(compositor: &Compositor, origin: (i32, i32)) -> EmojiPlace {
        let (ox, oy) = origin;
        let outputs: Vec<Rect> = compositor
            .outputs()
            .into_iter()
            .filter(|output| output.enabled)
            .filter_map(|output| output.rect)
            .map(|(x, y, width, height)| {
                let (x, y) = (x.saturating_sub(ox), y.saturating_sub(oy));
                Rect::new(
                    f64::from(x),
                    f64::from(y),
                    f64::from(x.saturating_add(width)),
                    f64::from(y.saturating_add(height)),
                )
            })
            .collect();
        let caret = compositor.text_caret().map(|caret| {
            Rect::new(
                caret.x - f64::from(ox),
                caret.y - f64::from(oy),
                caret.x - f64::from(ox) + caret.width,
                caret.y - f64::from(oy) + caret.height,
            )
        });
        let viewport = caret.and_then(|caret| {
            outputs
                .iter()
                .copied()
                .find(|display| display.contains(caret.center()))
        });
        EmojiPlace { viewport, caret }
    }
}

/// The picker's recents as this user's private file.
#[derive(Clone, Debug)]
struct FileRecentsStore {
    path: PathBuf,
    /// What the file held when the session started: the loop reads no path,
    /// so the store is read once while the session is still setting up.
    recents: Recents,
}

impl FileRecentsStore {
    fn at(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let recents = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self { path, recents }
    }

    /// `$XDG_STATE_HOME/tilo/emoji-recents.json`, or `None` without a user
    /// state directory.
    fn for_user() -> Option<Self> {
        tilo_paths::state_dir().map(|root| Self::at(root.join("tilo/emoji-recents.json")))
    }
}

impl RecentsStore for FileRecentsStore {
    /// What the file held at startup; a missing or malformed file reads as
    /// none.
    fn load(&self) -> Recents {
        self.recents.clone()
    }

    /// A pick happens between two frames, so the file is kept by the state
    /// writer: its fsyncs can take seconds under I/O pressure.
    fn save(&self, recents: &Recents) -> Result<(), String> {
        let bytes = serde_json::to_vec(recents)
            .map_err(|error| format!("{}: {error}", self.path.display()))?;
        tilo_durable_writer::keep(self.path.clone(), bytes);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn recents_persist_privately() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("state/tilo/emoji-recents.json");
        let store = FileRecentsStore::at(&path);
        let mut recents = Recents::default();
        recents.record("👍");
        recents.record("🎉");
        store.save(&recents).expect("saved");
        tilo_durable_writer::global().flush();
        let mode = std::fs::metadata(&store.path)
            .expect("saved")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(
            FileRecentsStore::at(&store.path).load().items(),
            ["🎉".to_string(), "👍".to_string()]
        );
    }

    /// A hand-edited or malformed file reads as no recents at all, rather
    /// than stopping the session.
    #[test]
    fn a_malformed_file_reads_as_none() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("emoji-recents.json");
        std::fs::write(&path, b"not json").expect("written");
        assert!(FileRecentsStore::at(&path).load().items().is_empty());
    }

    /// Picking an emoji happens between two frames; saving the recents must
    /// not wait for the disk there.
    #[cfg(target_os = "linux")]
    #[test]
    fn saving_a_pick_never_waits_for_the_disk() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = FileRecentsStore::at(dir.path().join("state/tilo/emoji-recents.json"));
        // The writer's thread is started before the filter goes on: it is the
        // one that syncs.
        let writer = tilo_durable_writer::global();
        let saving = store.clone();
        tilo_test_env::disk_sync::on_a_thread_that_cannot_sync(move || {
            let mut recents = Recents::default();
            recents.record("🦀");
            saving.save(&recents).expect("queued");
        });
        writer.flush();
        assert_eq!(
            FileRecentsStore::at(&store.path).load().items(),
            ["🦀".to_string()]
        );
    }
}
